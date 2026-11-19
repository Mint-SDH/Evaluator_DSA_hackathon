use crate::models::{IndividualReport, MasterSummaryRow, SectionSummaryRow};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Generates a clean, comprehensive text report for an individual student submission.
/// Includes student metadata, compilation details, a test summary table, and at the bottom,
/// the full outputs (actual stdout, expected stdout, and stderr) for all test cases.
pub fn generate_student_txt_report(report: &IndividualReport) -> String {
    let mut out = String::new();
    out.push_str("================================================================================\n");
    out.push_str("                       INDIVIDUAL STUDENT EVALUATION REPORT                     \n");
    out.push_str("================================================================================\n");
    out.push_str(&format!("Student Name       : {}\n", report.student_name));
    out.push_str(&format!("Section            : {}\n", report.section.as_deref().unwrap_or("Default")));
    out.push_str(&format!("Lab Number         : {}\n", report.lab_number));
    out.push_str(&format!("Problem ID         : {}\n", report.problem_id));
    out.push_str(&format!("Zip File           : {}\n", report.zip_filename));
    out.push_str(&format!("Submission Time    : {}\n", report.submission_time));
    out.push_str(&format!("Compilation Status : {}\n", if report.compiled { "SUCCESS" } else { "FAILED" }));
    out.push_str(&format!("Score              : {:.2} / {:.2} ({:.2}%)\n", report.total_score, report.max_score, report.percentage));
    out.push_str(&format!("Passed Test Cases  : {} / {}\n", report.tests_passed, report.total_tests));

    let overall_verdict = if !report.compiled {
        "COMPILATION ERROR"
    } else if report.tests_passed == report.total_tests && report.total_tests > 0 {
        "ACCEPTED (100%)"
    } else if report.tests_passed > 0 {
        "PARTIAL PASS"
    } else {
        "WRONG ANSWER (0%)"
    };
    out.push_str(&format!("Overall Verdict    : {}\n", overall_verdict));

    if let Some(ref err) = report.compilation_error {
        out.push_str("\n--------------------------------------------------------------------------------\n");
        out.push_str("COMPILATION ERROR DETAILS\n");
        out.push_str("--------------------------------------------------------------------------------\n");
        out.push_str(err);
        if !err.ends_with('\n') {
            out.push('\n');
        }
    }

    out.push_str("\n--------------------------------------------------------------------------------\n");
    out.push_str("TEST CASES SUMMARY\n");
    out.push_str("--------------------------------------------------------------------------------\n");
    if report.results.is_empty() {
        out.push_str("(No test cases executed)\n");
    } else {
        for (idx, tc) in report.results.iter().enumerate() {
            let status = if tc.passed { "PASSED" } else { "FAILED" };
            let note_suffix = if let Some(ref note) = tc.error_message {
                format!(" [{}]", note)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "[{:>2}] TC #{}: {:<32} | Status: {:<6} | Score: {:>5.1}/{:<5.1} | Time: {:>4}ms | Verdict: {}{}\n",
                idx + 1,
                tc.test_case_id,
                tc.test_case_name,
                status,
                tc.score,
                tc.max_score,
                tc.execution_time_ms,
                tc.verdict,
                note_suffix
            ));
        }
    }

    out.push_str("\n================================================================================\n");
    out.push_str("                         ALL TEST CASE TERMINAL OUTPUTS                         \n");
    out.push_str("================================================================================\n");

    if report.results.is_empty() {
        out.push_str("(No test case outputs available)\n");
    } else {
        for (idx, tc) in report.results.iter().enumerate() {
            out.push_str(&format!(
                "\n--------------------------------------------------------------------------------\n\
                 >>> TEST CASE {} (ID: {}): {}\n\
                 Status: {} | Verdict: {} | Score: {:.1}/{:.1} | Time: {}ms\n",
                idx + 1,
                tc.test_case_id,
                tc.test_case_name,
                if tc.passed { "PASSED" } else { "FAILED" },
                tc.verdict,
                tc.score,
                tc.max_score,
                tc.execution_time_ms
            ));
            if let Some(ref note) = tc.error_message {
                out.push_str(&format!("Evaluation Note: {}\n", note));
            }
            out.push_str("--------------------------------------------------------------------------------\n");

            out.push_str("--- ACTUAL TERMINAL OUTPUT ---\n");
            if tc.actual_stdout.trim().is_empty() {
                out.push_str("(No stdout output produced)\n");
            } else {
                out.push_str(&tc.actual_stdout);
                if !tc.actual_stdout.ends_with('\n') {
                    out.push('\n');
                }
            }

            out.push_str("\n--- EXPECTED TERMINAL OUTPUT ---\n");
            if tc.expected_stdout.trim().is_empty() {
                out.push_str("(No stdout expected)\n");
            } else {
                out.push_str(&tc.expected_stdout);
                if !tc.expected_stdout.ends_with('\n') {
                    out.push('\n');
                }
            }

            if !tc.stderr.trim().is_empty() {
                out.push_str("\n--- STDERR / ERROR ---\n");
                out.push_str(&tc.stderr);
                if !tc.stderr.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }

    out.push_str("\n================================================================================\n");
    out.push_str("                            END OF REPORT                                       \n");
    out.push_str("================================================================================\n");

    out
}

/// Writes the detailed .txt report for a single student submission, segregated by section.
/// Reports are saved in <output_dir>/student_reports/<section_name>/<zip_stem>_report.txt
/// (or <output_dir>/student_reports/<zip_stem>_report.txt if no section).
pub fn write_student_report(
    output_dir: &Path,
    section_name: Option<&str>,
    report: &IndividualReport,
    zip_stem: &str,
) -> io::Result<PathBuf> {
    let base_reports_dir = output_dir.join("student_reports");
    let target_dir = if let Some(sec) = section_name {
        base_reports_dir.join(sec)
    } else {
        base_reports_dir
    };

    if !target_dir.exists() {
        std::fs::create_dir_all(&target_dir)?;
    }

    // Generate and write the comprehensive .txt report (only .txt format as requested)
    let txt_filename = format!("{zip_stem}_report.txt");
    let txt_path = target_dir.join(txt_filename);
    let txt_content = generate_student_txt_report(report);

    let mut txt_file = File::create(&txt_path)?;
    txt_file.write_all(txt_content.as_bytes())?;
    txt_file.flush()?;

    Ok(txt_path)
}

/// Writes the section CSV report inside <output_dir>/csv_reports/<section_name>.csv
/// Columns:
/// student_name, total_test_cases, passed_test_cases, percentage_marks, scaled_down, fixed_marks, aloted_marks
pub fn write_section_csv(
    output_dir: &Path,
    section_name: &str,
    rows: &[SectionSummaryRow],
) -> io::Result<PathBuf> {
    let csv_dir = output_dir.join("csv_reports");
    if !csv_dir.exists() {
        std::fs::create_dir_all(&csv_dir)?;
    }

    let csv_filename = format!("{section_name}.csv");
    let default_csv_path = csv_dir.join(&csv_filename);
    let (file, target_path) = open_csv_resiliently(&csv_dir, &default_csv_path)?;

    let mut writer = csv::Writer::from_writer(file);

    // Required columns:
    // student_name, total_test_cases, passed_test_cases, percentage_marks, scaled_down, fixed_marks, aloted_marks
    writer.write_record([
        "student_name",
        "total_test_cases",
        "passed_test_cases",
        "percentage_marks",
        "scaled_down",
        "fixed_marks",
        "aloted_marks",
    ])?;

    for row in rows {
        writer.write_record(&[
            &row.student_name,
            &row.total_test_cases.to_string(),
            &row.passed_test_cases.to_string(),
            &row.percentage_marks.to_string(),
            &row.scaled_down.to_string(),
            &row.fixed_marks.to_string(),
            &row.aloted_marks.to_string(),
        ])?;
    }

    writer.flush()?;
    Ok(target_path)
}

/// Writes the master summary CSV aggregating results across all sections.
pub fn write_master_csv(
    output_dir: &Path,
    rows: &[MasterSummaryRow],
) -> io::Result<PathBuf> {
    if !output_dir.exists() {
        std::fs::create_dir_all(output_dir)?;
    }

    let csv_dir = output_dir.join("csv_reports");
    if !csv_dir.exists() {
        std::fs::create_dir_all(&csv_dir)?;
    }

    // Write to both csv_reports/master_report.csv and output/master_report.csv for accessibility
    let csv_path = csv_dir.join("master_report.csv");
    let (file, target_path) = open_csv_resiliently(&csv_dir, &csv_path)?;

    let mut writer = csv::Writer::from_writer(file);

    let headers = [
        "student_name",
        "section",
        "lab_number",
        "submission_time",
        "zip_filename",
        "status",
        "passed_tests",
        "total_tests",
        "execution_time_total_ms",
        "notes",
        "scaled_down",
        "fixed_marks",
        "aloted_marks",
    ];

    writer.write_record(&headers)?;

    for row in rows {
        writer.write_record(&[
            &row.student_name,
            row.section.as_deref().unwrap_or("Default"),
            &row.lab_number,
            &row.submission_time,
            &row.zip_filename,
            &row.status,
            &row.passed_tests.to_string(),
            &row.total_tests.to_string(),
            &row.execution_time_total_ms.to_string(),
            &row.notes,
            &row.scaled_down.to_string(),
            &row.fixed_marks.to_string(),
            &row.aloted_marks.to_string(),
        ])?;
    }

    writer.flush()?;

    // Also write a copy to output_dir/master_report.csv if available
    let root_csv = output_dir.join("master_report.csv");
    if let Ok((root_file, _)) = open_csv_resiliently(output_dir, &root_csv) {
        let mut root_writer = csv::Writer::from_writer(root_file);
        let _ = root_writer.write_record(&headers);
        for row in rows {
            let _ = root_writer.write_record(&[
                &row.student_name,
                row.section.as_deref().unwrap_or("Default"),
                &row.lab_number,
                &row.submission_time,
                &row.zip_filename,
                &row.status,
                &row.passed_tests.to_string(),
                &row.total_tests.to_string(),
                &row.execution_time_total_ms.to_string(),
                &row.notes,
                &row.scaled_down.to_string(),
                &row.fixed_marks.to_string(),
                &row.aloted_marks.to_string(),
            ]);
        }
        let _ = root_writer.flush();
    }

    Ok(target_path)
}

fn open_csv_resiliently(output_dir: &Path, target: &Path) -> io::Result<(File, PathBuf)> {
    for attempt in 0..5 {
        match File::create(target) {
            Ok(f) => return Ok((f, target.to_path_buf())),
            Err(e) if e.raw_os_error() == Some(32) || e.kind() == io::ErrorKind::PermissionDenied => {
                std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let file_stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("report");
    let fallback_name = format!("{file_stem}_{timestamp}.csv");
    let fallback_path = output_dir.join(&fallback_name);
    eprintln!(
        "\nNotice: '{}' is locked by another process.\nSaving summary to '{}' instead.\n",
        target.display(),
        fallback_path.display()
    );
    let f = File::create(&fallback_path)?;
    Ok((f, fallback_path))
}
