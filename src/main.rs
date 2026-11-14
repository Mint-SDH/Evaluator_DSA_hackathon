use c_autograder::compiler;
use c_autograder::config::CliArgs;
use c_autograder::extractor;
use c_autograder::models::{IndividualReport, MasterSummaryRow, TestCaseResult, TestSuite};
use c_autograder::platform;
use c_autograder::reporter;
use c_autograder::runner;

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

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

    // Apply CLI lab override if specified
    if let Some(ref lab_override) = cli.lab {
        test_suite.lab_number = Some(lab_override.clone());
    }

    let problem_id = test_suite.problem_id.clone();
    let lab_override = test_suite.lab_number.clone();
    let total_tests_count = test_suite.test_cases.len();

    println!("Loaded Test Suite:");
    println!("  Problem ID:  {}", problem_id);
    println!("  Lab Number:  {}", lab_override.as_deref().unwrap_or("Default"));
    println!("  Test Cases:  {}", total_tests_count);
    println!("  Time Limit:  {} ms", test_suite.time_limit_ms.unwrap_or(2000));
    println!("  Zips Dir:    {}", cli.zips_dir.display());
    println!("  Output Dir:  {}", cli.output_dir.display());

    // 3. Setup directories
    let output_dir = &cli.output_dir;
    let sandbox_dir = output_dir.join(".temp_sandbox");
    let student_reports_dir = output_dir.join("student_reports");

    std::fs::create_dir_all(&sandbox_dir)?;
    std::fs::create_dir_all(&student_reports_dir)?;

    // 4. Configure Rayon thread pool if specified
    if let Some(jobs) = cli.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()
            .unwrap_or_else(|e| eprintln!("Warning: Rayon thread pool already configured: {e}"));
        println!("  Workers:     {} parallel threads", jobs);
    } else {
        println!("  Workers:     {} parallel threads (system cores)", rayon::current_num_threads());
    }

    // 5. Discover student .zip files (read-only discovery)
    let mut zip_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&cli.zips_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if ext.eq_ignore_ascii_case("zip") {
                    zip_files.push(path);
                }
            }
        }
    }

    let total_submissions = zip_files.len();
    if total_submissions == 0 {
        println!("\nNo .zip archives found in '{}'. Exiting.", cli.zips_dir.display());
        let _ = platform::robust_remove_path(&sandbox_dir);
        return Ok(());
    }

    println!("\nDiscovered {} submission archives. Commencing parallel evaluation...\n", total_submissions);

    // 6. Setup progress bar
    let pb = ProgressBar::new(total_submissions as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
            .expect("Valid progress bar template")
            .progress_chars("#>-"),
    );

    // 7. Parallel Evaluation Pipeline
    let mut summary_rows: Vec<MasterSummaryRow> = zip_files
        .par_iter()
        .map(|zip_path| {
            let zip_stem = zip_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let row = evaluate_single_submission(
                zip_path,
                &zip_stem,
                &sandbox_dir,
                output_dir,
                &test_suite,
                lab_override.as_deref(),
            );

            pb.inc(1);
            pb.set_message(format!("Evaluated {}", row.student_name));
            row
        })
        .collect();

    pb.finish_with_message("Batch evaluation finished!");

    // 8. Sort summary rows by student_name then submission_time
    summary_rows.sort_by(|a, b| {
        a.student_name
            .cmp(&b.student_name)
            .then_with(|| a.submission_time.cmp(&b.submission_time))
    });

    // 9. Write Master CSV Report
    let master_csv_path = reporter::write_master_csv(output_dir, &summary_rows)?;

    // 10. Clean up empty sandbox directory
    let _ = platform::robust_remove_path(&sandbox_dir);

    // 11. Final Summary Statistics
    let elapsed = start_time.elapsed();
    let compiled_count = summary_rows.iter().filter(|r| r.status == "COMPILED").count();
    let perfect_count = summary_rows
        .iter()
        .filter(|r| r.status == "COMPILED" && r.percentage >= 100.0)
        .count();
    let total_score_sum: f64 = summary_rows.iter().map(|r| r.score).sum();
    let avg_score = if total_submissions > 0 {
        total_score_sum / (total_submissions as f64)
    } else {
        0.0
    };

    println!("\n============================================================");
    println!("                  EVALUATION COMPLETE                       ");
    println!("============================================================");
    println!("Total Submissions:     {}", total_submissions);
    println!("Successfully Compiled: {} ({:.1}%)", compiled_count, (compiled_count as f64 / total_submissions as f64) * 100.0);
    println!("Perfect Scores (100%): {}", perfect_count);
    println!("Average Score:         {:.2}", avg_score);
    println!("Total Execution Time:  {:.2?}", elapsed);
    println!("Master Report CSV:     {}", master_csv_path.display());
    println!("Individual Reports:    {}", student_reports_dir.display());
    println!("============================================================");

    Ok(())
}

/// Evaluates a single student submission and immediately cleans up the ephemeral extraction directory.
fn evaluate_single_submission(
    zip_path: &Path,
    zip_stem: &str,
    sandbox_dir: &Path,
    output_dir: &Path,
    test_suite: &TestSuite,
    lab_override: Option<&str>,
) -> MasterSummaryRow {
    // 1. Read-only extraction into sandbox
    let extraction_res = extractor::extract_submission(zip_path, sandbox_dir, lab_override);

    let extracted = match extraction_res {
        Ok(ext) => ext,
        Err(e) => {
            let filename = zip_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown.zip".to_string());
            return MasterSummaryRow {
                student_name: zip_stem.to_string(),
                lab_number: lab_override.unwrap_or("Lab").to_string(),
                submission_time: "N/A".to_string(),
                zip_filename: filename,
                status: "EXTRACTION_FAILED".to_string(),
                score: 0.0,
                max_score: 0.0,
                percentage: 0.0,
                passed_tests: 0,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: 0,
                notes: format!("Extraction Error: {e}"),
            };
        }
    };

    let metadata = extracted.metadata.clone();

    // 2. Cross-platform C Compilation
    let compilation_res = compiler::compile_submission(&extracted.work_dir);

    let (individual_report, summary_row) = match compilation_res {
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

            let row = MasterSummaryRow {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                status: "COMPILED".to_string(),
                score: total_score,
                max_score,
                percentage,
                passed_tests: passed_count,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: total_time_ms,
                notes,
            };

            (report, row)
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

            let row = MasterSummaryRow {
                student_name: metadata.student_name.clone(),
                lab_number: metadata.lab_number.clone(),
                submission_time: metadata.submission_time.clone(),
                zip_filename: metadata.zip_filename.clone(),
                status: "COMPILATION_ERROR".to_string(),
                score: 0.0,
                max_score: max_possible_score,
                percentage: 0.0,
                passed_tests: 0,
                total_tests: test_suite.test_cases.len(),
                execution_time_total_ms: 0,
                notes: note,
            };

            (report, row)
        }
    };

    // 4. Write Individual JSON Report
    let _ = reporter::write_student_report(output_dir, &individual_report, zip_stem);

    // 5. CRITICAL: Immediately delete extracted folder and compiled binary
    // Uses platform::robust_cleanup with retry loop for Windows NTFS locking
    let _ = platform::robust_cleanup(extracted.temp_dir);

    summary_row
}

/// Extracts a concise, informative compiler error line from GCC or make output.
/// Prioritizes specific compiler errors (e.g. "trie.c:38: error: expected ';'") and linker errors
/// (e.g. "process_text.c: undefined reference to 'word_to_lower'"), cleaning temporary sandbox paths.
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

/// Cleans temporary directory prefixes and GCC temp object prefixes from error strings so reports display cleanly,
/// e.g. "C:\...\temp\4\process_text.c:38: error: ..." -> "process_text.c:38: error: ..."
/// e.g. "C:\...\ccNxj6NU.o:process_text.c:...: undefined reference" -> "process_text.c:...: undefined reference"
fn clean_error_path(line: &str) -> String {
    // Strip temporary object file prefix like ccNxj6NU.o:
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
/// Example: "0/4 Tests Passed (4 TLE) | Failed: TC1(TLE), TC2(TLE), TC3(TLE), TC4(TLE)"
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
