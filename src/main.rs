use c_autograder::compiler;
use c_autograder::config::CliArgs;
use c_autograder::extractor;
use c_autograder::models::{
    IndividualReport, MasterSummaryRow, SectionSummaryRow, TestCaseResult, TestSuite,
};
use c_autograder::platform;
use c_autograder::reporter;
use c_autograder::runner;

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Represents a discovered student submission tagged with its section/class folder.
struct SubmissionTarget {
    section_name: String,
    zip_path: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    platform::init_platform();
    let start_time = Instant::now();
    let cli = CliArgs::parse();

    println!("============================================================");
    println!("             C-AUTOGRADER (Cross-Platform)                  ");
    println!("     High-Performance Batch Evaluation Engine for C         ");
    println!("============================================================");

    // 1. Validate inputs
    if !cli.zips_dir.is_dir() {
        eprintln!(
            "Error: Submissions directory '{}' does not exist or is not a directory.",
            cli.zips_dir.display()
        );
        std::process::exit(1);
    }

    if !cli.testcases.is_file() {
        eprintln!(
            "Error: Test cases file '{}' does not exist or is not a file.",
            cli.testcases.display()
        );
        std::process::exit(1);
    }

    // 2. Parse test suite JSON
    let testcases_file = File::open(&cli.testcases)?;
    let mut test_suite: TestSuite = serde_json::from_reader(testcases_file)
        .map_err(|e| format!("Failed to parse test suite JSON: {e}"))?;

    // Enforce strict lab_number resolution:
    // Priority 1 (Explicit CLI Override): User passed `--lab <VALUE>`
    // Priority 2 (testcases.json Field): `test_suite.lab_number` directly from JSON
    let cli_override = cli
        .lab
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let json_lab = test_suite
        .lab_number
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let resolved_base_lab = cli_override.or(json_lab);
    test_suite.lab_number = resolved_base_lab.clone();

    let problem_id = test_suite.problem_id.clone();
    let display_lab = resolved_base_lab
        .as_deref()
        .unwrap_or("Inferred from Submissions");
    let total_tests_count = test_suite.test_cases.len();

    println!("Loaded Test Suite:");
    println!("  Problem ID:  {}", problem_id);
    println!("  Lab Number:  {}", display_lab);
    println!("  Test Cases:  {}", total_tests_count);
    println!("  Time Limit:  {} ms", test_suite.time_limit_ms.unwrap_or(2000));
    println!("  Zips Dir:    {}", cli.zips_dir.display());
    println!("  Output Dir:  {}", cli.output_dir.display());

    // 3. Setup directories
    let output_dir = &cli.output_dir;
    let sandbox_dir = output_dir.join(".temp_sandbox");
    let student_reports_dir = output_dir.join("student_reports");
    let csv_reports_dir = output_dir.join("csv_reports");

    std::fs::create_dir_all(&sandbox_dir)?;
    std::fs::create_dir_all(&student_reports_dir)?;
    std::fs::create_dir_all(&csv_reports_dir)?;

    // 4. Configure high-performance Rayon thread pool to fully utilize hardware
    let num_threads = if let Some(jobs) = cli.jobs {
        jobs
    } else {
        let logical_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        // Optimize for hybrid process-spawning and I/O autograding workload:
        // 2x logical cores prevents worker starvation while threads wait on external make/gcc processes
        std::cmp::max(logical_cores * 2, 16)
    };

    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .unwrap_or_else(|e| eprintln!("Warning: Rayon thread pool already configured: {e}"));

    println!(
        "  Workers:     {} parallel worker threads (maximizing laptop CPU & I/O throughput)",
        num_threads
    );

    // 5. Discover student .zip files across section subdirectories or root folder
    let targets = discover_submission_targets(&cli.zips_dir)?;
    let total_submissions = targets.len();

    if total_submissions == 0 {
        println!(
            "\nNo .zip archives found in '{}'. Exiting.",
            cli.zips_dir.display()
        );
        let _ = platform::robust_remove_path(&sandbox_dir);
        return Ok(());
    }

    let unique_sections: HashSet<_> = targets.iter().map(|t| &t.section_name).collect();
    println!(
        "\nDiscovered {} submission archives across {} sections/classes. Commencing parallel evaluation...\n",
        total_submissions,
        unique_sections.len()
    );

    // 6. Setup progress bar
    let pb = ProgressBar::new(total_submissions as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
            .expect("Valid progress bar template")
            .progress_chars("#>-"),
    );

    // 7. Parallel Evaluation Pipeline across all sections
    let evaluated_results: Vec<(String, MasterSummaryRow, SectionSummaryRow)> = targets
        .par_iter()
        .map(|target| {
            let zip_stem = target
                .zip_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let (master_row, section_row) = evaluate_single_submission(
                target,
                &zip_stem,
                &sandbox_dir,
                output_dir,
                &test_suite,
                resolved_base_lab.as_deref(),
            );

            pb.inc(1);
            pb.set_message(format!("[{}] {}", target.section_name, master_row.student_name));
            (target.section_name.clone(), master_row, section_row)
        })
        .collect();

    pb.finish_with_message("Batch evaluation finished!");

    // 8. Segregate results by section and generate 1 CSV per section
    let mut section_map: BTreeMap<String, Vec<SectionSummaryRow>> = BTreeMap::new();
    let mut master_rows: Vec<MasterSummaryRow> = Vec::with_capacity(evaluated_results.len());

    for (section_name, master_row, section_row) in evaluated_results {
        section_map
            .entry(section_name)
            .or_default()
            .push(section_row);
        master_rows.push(master_row);
    }

    println!("\nGenerating section CSV reports in '{}'...", csv_reports_dir.display());
    for (section_name, mut section_rows) in section_map {
        // Sort each section's CSV alphabetically by student_name
        section_rows.sort_by(|a, b| a.student_name.cmp(&b.student_name));
        let section_csv = reporter::write_section_csv(output_dir, &section_name, &section_rows)?;
        println!(
            "  - {} ({} submissions) -> {}",
            section_name,
            section_rows.len(),
            section_csv.display()
        );
    }

    // 9. Write Master Combined CSV Report
    master_rows.sort_by(|a, b| {
        a.section
            .cmp(&b.section)
            .then_with(|| a.student_name.cmp(&b.student_name))
            .then_with(|| a.submission_time.cmp(&b.submission_time))
    });
    let master_csv_path = reporter::write_master_csv(output_dir, &master_rows)?;

    // 10. Clean up empty sandbox directory
    let _ = platform::robust_remove_path(&sandbox_dir);

    // 11. Final Summary Statistics
    let elapsed = start_time.elapsed();
    let compiled_count = master_rows.iter().filter(|r| r.status == "COMPILED").count();
    let perfect_count = master_rows
        .iter()
        .filter(|r| r.status == "COMPILED" && r.percentage >= 100.0)
        .count();
    let total_score_sum: f64 = master_rows.iter().map(|r| r.score).sum();
    let avg_score = if total_submissions > 0 {
        total_score_sum / (total_submissions as f64)
    } else {
        0.0
    };

    println!("\n============================================================");
    println!("                  EVALUATION COMPLETE                       ");
    println!("============================================================");
    println!("Total Submissions:     {} across {} sections", total_submissions, unique_sections.len());
    println!(
        "Successfully Compiled: {} ({:.1}%)",
        compiled_count,
        (compiled_count as f64 / total_submissions as f64) * 100.0
    );
    println!("Perfect Scores (100%): {}", perfect_count);
    println!("Average Score:         {:.2}%", avg_score);
    println!("Total Execution Time:  {:.2?}", elapsed);
    println!("Section CSV Reports:   {}", csv_reports_dir.display());
    println!("Individual Student .txt Reports: {} (segregated by section)", student_reports_dir.display());
    println!("Master Combined CSV:   {}", master_csv_path.display());
    println!("============================================================");

    Ok(())
}

/// Discovers submission .zip archives, supporting:
/// 1. Section subdirectories: submissions_zip/<Section_Name>/<student>.zip
/// 2. Root directory zips: submissions_zip/<student>.zip
fn discover_submission_targets(zips_dir: &Path) -> Result<Vec<SubmissionTarget>, Box<dyn std::error::Error>> {
    let mut targets = Vec::new();
    let mut root_zips = Vec::new();

    for entry in std::fs::read_dir(zips_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let section_name = entry.file_name().to_string_lossy().to_string();
            // Ignore hidden directories like .git or .temp_sandbox
            if section_name.starts_with('.') {
                continue;
            }
            for sub_entry in std::fs::read_dir(&path)? {
                let sub_entry = sub_entry?;
                let sub_path = sub_entry.path();
                if sub_path.is_file() {
                    if let Some(ext) = sub_path.extension().and_then(|s| s.to_str()) {
                        if ext.eq_ignore_ascii_case("zip") {
                            targets.push(SubmissionTarget {
                                section_name: section_name.clone(),
                                zip_path: sub_path,
                            });
                        }
                    }
                }
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if ext.eq_ignore_ascii_case("zip") {
                    root_zips.push(path);
                }
            }
        }
    }

    // If zips are placed directly in zips_dir, group them under the folder's name
    if !root_zips.is_empty() {
        let default_section = zips_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "submissions".to_string());
        for zip in root_zips {
            targets.push(SubmissionTarget {
                section_name: default_section.clone(),
                zip_path: zip,
            });
        }
    }

    targets.sort_by(|a, b| {
        a.section_name
            .cmp(&b.section_name)
            .then_with(|| a.zip_path.cmp(&b.zip_path))
    });

    Ok(targets)
}

/// Evaluates a single student submission, writes its detailed report into the section folder,
/// and immediately cleans up the ephemeral extraction directory.
fn evaluate_single_submission(
    target: &SubmissionTarget,
    zip_stem: &str,
    sandbox_dir: &Path,
    output_dir: &Path,
    test_suite: &TestSuite,
    base_lab: Option<&str>,
) -> (MasterSummaryRow, SectionSummaryRow) {
    let zip_path = &target.zip_path;
    let section_name = &target.section_name;

    // 1. Read-only extraction into sandbox (enforces 4-tier lab_number resolution)
    let extraction_res = extractor::extract_submission(zip_path, sandbox_dir, base_lab);

    let extracted = match extraction_res {
        Ok(ext) => ext,
        Err(e) => {
            let filename = zip_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown.zip".to_string());
            let fallback_lab = extractor::resolve_lab_number(base_lab, None, zip_stem);

            let individual_report = IndividualReport {
                student_name: zip_stem.to_string(),
                lab_number: fallback_lab.clone(),
                submission_time: "N/A".to_string(),
                zip_filename: filename.clone(),
                section: Some(section_name.clone()),
                problem_id: test_suite.problem_id.clone(),
                compiled: false,
                compilation_error: Some(format!("Extraction Error: {e} (Archive could not be unpacked; verify that submission is a valid standard .zip archive and not a renamed .rar or invalid format)")),
                total_score: 0.0,
                max_score: 0.0,
                percentage: 0.0,
                tests_passed: 0,
                total_tests: test_suite.test_cases.len(),
                results: vec![],
            };
            let _ = reporter::write_student_report(output_dir, Some(section_name), &individual_report, zip_stem);

            let scaled_down: u64 = 0;
            let fixed_marks: u64 = 3;
            let aloted_marks: u64 = scaled_down + fixed_marks;

            let master_row = MasterSummaryRow {
                student_name: zip_stem.to_string(),
                lab_number: fallback_lab,
                submission_time: "N/A".to_string(),
                zip_filename: filename,
                section: Some(section_name.clone()),
                status: "EXTRACTION_FAILED".to_string(),
                score: 0.0,
                max_score: 0.0,
                percentage: 0.0,
                passed_tests: 0,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: 0,
                notes: format!("Extraction Error: {e}"),
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            let section_row = SectionSummaryRow {
                student_name: zip_stem.to_string(),
                total_test_cases: test_suite.test_cases.len(),
                passed_test_cases: 0,
                percentage_marks: 0,
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            return (master_row, section_row);
        }
    };

    let metadata = extracted.metadata.clone();

    // 2. Cross-platform C Compilation
    let compilation_res = compiler::compile_submission(&extracted.work_dir);

    let (individual_report, master_row, section_row) = match compilation_res {
        Ok(bin_path) => {
            // 3. Execution & Testing
            let (results, total_score, max_score, passed_count, total_time_ms) =
                runner::run_test_suite(&bin_path, &extracted.work_dir, test_suite);

            let percentage = if max_score > 0.0 {
                (total_score / max_score) * 100.0
            } else {
                0.0
            };

            let notes = format_test_run_notes(passed_count, test_suite.test_cases.len(), &results);

            let report = IndividualReport {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                section: Some(section_name.clone()),
                problem_id: test_suite.problem_id.clone(),
                compiled: true,
                compilation_error: None,
                total_score,
                max_score,
                percentage,
                tests_passed: passed_count,
                total_tests: test_suite.test_cases.len(),
                results,
            };

            let scaled_down: u64 = (passed_count as f64 / 2.0).ceil() as u64;
            let fixed_marks: u64 = 3;
            let aloted_marks: u64 = scaled_down + fixed_marks;

            let row = MasterSummaryRow {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                section: Some(section_name.clone()),
                status: "COMPILED".to_string(),
                score: total_score,
                max_score,
                percentage,
                passed_tests: passed_count,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: total_time_ms,
                notes,
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            let integer_percentage = if test_suite.test_cases.is_empty() {
                0
            } else {
                ((passed_count as f64 / test_suite.test_cases.len() as f64) * 100.0).round() as u64
            };

            let sec_row = SectionSummaryRow {
                student_name: zip_stem.to_string(), // name of the zip folder
                total_test_cases: test_suite.test_cases.len(),
                passed_test_cases: passed_count,
                percentage_marks: integer_percentage,
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            (report, row, sec_row)
        }
        Err(err_msg) => {
            // Compilation failed
            let max_possible_score: f64 = test_suite.test_cases.iter().map(|tc| tc.weight).sum();
            let failed_results: Vec<TestCaseResult> = test_suite
                .test_cases
                .iter()
                .map(|tc| TestCaseResult {
                    test_case_id: tc.id.clone(),
                    test_case_name: tc.name.clone(),
                    verdict: "Compilation Error".to_string(),
                    passed: false,
                    score: 0.0,
                    max_score: tc.weight,
                    execution_time_ms: 0,
                    actual_stdout: String::new(),
                    expected_stdout: tc.expected_stdout.clone(),
                    stderr: err_msg.clone(),
                    error_message: Some("Compilation failed".to_string()),
                })
                .collect();

            let report = IndividualReport {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                section: Some(section_name.clone()),
                problem_id: test_suite.problem_id.clone(),
                compiled: false,
                compilation_error: Some(err_msg.clone()),
                total_score: 0.0,
                max_score: max_possible_score,
                percentage: 0.0,
                tests_passed: 0,
                total_tests: test_suite.test_cases.len(),
                results: failed_results,
            };

            let note = format_compilation_note(&err_msg);

            let scaled_down: u64 = 0;
            let fixed_marks: u64 = 3;
            let aloted_marks: u64 = scaled_down + fixed_marks;

            let row = MasterSummaryRow {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                section: Some(section_name.clone()),
                status: "COMPILATION_ERROR".to_string(),
                score: 0.0,
                max_score: max_possible_score,
                percentage: 0.0,
                passed_tests: 0,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: 0,
                notes: note,
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            let sec_row = SectionSummaryRow {
                student_name: zip_stem.to_string(),
                total_test_cases: test_suite.test_cases.len(),
                passed_test_cases: 0,
                percentage_marks: 0,
                scaled_down,
                fixed_marks,
                aloted_marks,
            };

            (report, row, sec_row)
        }
    };

    // 4. Write Individual JSON Report segregated by section
    let _ = reporter::write_student_report(output_dir, Some(section_name), &individual_report, zip_stem);

    // 5. CRITICAL: Immediately delete extracted folder and compiled binary
    // Uses platform::robust_cleanup with retry loop for Windows NTFS locking
    let _ = platform::robust_cleanup(extracted.temp_dir);

    (master_row, section_row)
}

/// Extracts a concise, informative compiler error line from GCC or make output.
fn format_compilation_note(err_msg: &str) -> String {
    let mut specific_c_error = None;
    let mut undefined_ref_error = None;
    let mut generic_error = None;

    for raw_line in err_msg.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();

        // 1. Check for specific C compilation errors (e.g. "file.c:10:5: error: ...")
        if (lower.contains("error:") || lower.contains("fatal error:"))
            && !lower.contains("collect2")
            && !lower.contains("ld returned")
        {
            let clean_line = clean_error_path(line);
            specific_c_error = Some(clean_line);
            break;
        }

        // 2. Check for linker errors (e.g. undefined reference to `foo`)
        if lower.contains("undefined reference to") {
            let clean_line = clean_error_path(line);
            if undefined_ref_error.is_none() {
                undefined_ref_error = Some(clean_line);
            }
        }

        // 3. Generic error (e.g. collect2 ld returned 1 exit status)
        if lower.contains("error:") && generic_error.is_none() {
            generic_error = Some(line.to_string());
        }
    }

    if let Some(err) = specific_c_error {
        return format!("Compilation Failed: {err}");
    }
    if let Some(err) = undefined_ref_error {
        return format!("Compilation Failed: {err}");
    }
    if let Some(err) = generic_error {
        return format!("Compilation Failed: {err}");
    }

    // 4. Fallback: first non-header line
    for raw_line in err_msg.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Compilation Failed:")
            || line.starts_with("--- Stage")
            || line.starts_with("GCC exited with")
            || line.starts_with("Stderr:")
            || line.starts_with("Stdout:")
        {
            continue;
        }
        return format!("Compilation Failed: {}", clean_error_path(line));
    }

    "Compilation Failed".to_string()
}

/// Cleans temporary directory prefixes and GCC temp object prefixes from error strings so reports display cleanly.
fn clean_error_path(line: &str) -> String {
    let line_cleaned = if let Some(idx) = line.find(".o:") {
        &line[idx + 3..]
    } else {
        line
    };

    if let Some(idx) = line_cleaned.find(".c:") {
        let prefix = &line_cleaned[..idx];
        if let Some(sep) = prefix.rfind(|c| c == '/' || c == '\\') {
            return line_cleaned[sep + 1..].to_string();
        }
        return line_cleaned.to_string();
    } else if let Some(idx) = line_cleaned.find(".h:") {
        let prefix = &line_cleaned[..idx];
        if let Some(sep) = prefix.rfind(|c| c == '/' || c == '\\') {
            return line_cleaned[sep + 1..].to_string();
        }
        return line_cleaned.to_string();
    } else if line_cleaned.contains("undefined reference to") {
        if let Some(idx) = line_cleaned.find("undefined reference to") {
            let prefix = &line_cleaned[..idx];
            if let Some(sep) = prefix.rfind(|c| c == '/' || c == '\\') {
                return line_cleaned[sep + 1..].to_string();
            }
            return line_cleaned[idx..].to_string();
        }
    }
    line_cleaned.to_string()
}

/// Formats test execution notes distinguishing TLE, Crashes/Segfaults, and Wrong Answers.
fn format_test_run_notes(
    passed_count: usize,
    total_tests: usize,
    results: &[TestCaseResult],
) -> String {
    if passed_count == total_tests {
        return "All Tests Passed".to_string();
    }

    let mut tle_count = 0;
    let mut crash_count = 0;
    let mut wa_count = 0;
    let mut other_count = 0;
    let mut failed_tags = Vec::new();

    for tc in results {
        if tc.passed {
            continue;
        }

        let tag = if tc.verdict == "Time Limit Exceeded" {
            tle_count += 1;
            format!("TC{}(TLE)", tc.test_case_id)
        } else if tc.verdict.contains("Segmentation Fault")
            || tc.verdict.contains("Access Violation")
            || tc.verdict.contains("SIGSEGV")
        {
            crash_count += 1;
            format!("TC{}(Segfault)", tc.test_case_id)
        } else if tc.verdict.starts_with("Runtime Error") {
            crash_count += 1;
            format!("TC{}(Crash)", tc.test_case_id)
        } else if tc.verdict == "Wrong Answer" {
            wa_count += 1;
            format!("TC{}(WA)", tc.test_case_id)
        } else {
            other_count += 1;
            format!("TC{}(Fail)", tc.test_case_id)
        };

        failed_tags.push(tag);
    }

    let mut breakdown_parts = Vec::new();
    if tle_count > 0 {
        breakdown_parts.push(format!("{tle_count} TLE"));
    }
    if crash_count > 0 {
        breakdown_parts.push(format!("{crash_count} Crash"));
    }
    if wa_count > 0 {
        breakdown_parts.push(format!("{wa_count} WA"));
    }
    if other_count > 0 {
        breakdown_parts.push(format!("{other_count} Other"));
    }

    let breakdown_str = if breakdown_parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", breakdown_parts.join(", "))
    };

    let details_str = if failed_tags.len() <= 6 {
        failed_tags.join(", ")
    } else {
        format!("{}, ... (+{} more)", failed_tags[..5].join(", "), failed_tags.len() - 5)
    };

    format!("{passed_count}/{total_tests} Tests Passed{breakdown_str} | Failed: {details_str}")
}
