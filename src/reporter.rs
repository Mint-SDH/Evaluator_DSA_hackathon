use crate::models::{IndividualReport, MasterSummaryRow, SectionSummaryRow};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes the detailed JSON report for a single student submission, segregated by section.
/// Reports are saved in <output_dir>/student_reports/<section_name>/<zip_stem>_report.json
/// (or <output_dir>/student_reports/<zip_stem>_report.json if no section).
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

    let report_filename = format!("{zip_stem}_report.json");
    let report_path = target_dir.join(report_filename);

    let json_content = serde_json::to_string_pretty(report)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut file = File::create(&report_path)?;
    file.write_all(json_content.as_bytes())?;
    file.flush()?;

    Ok(report_path)
}

/// Writes the section CSV report inside <output_dir>/csv_reports/<section_name>.csv
/// The ONLY entries are:
/// student_name, total_test_cases, passed_test_cases, percentage_marks (integer)
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

    // Exact required columns:
    // student_name (i.e. name of the zip folder), total test cases, how many passed, integer % marks
    writer.write_record([
        "student_name",
        "total_test_cases",
        "passed_test_cases",
        "percentage_marks",
    ])?;

    for row in rows {
        writer.write_record(&[
            &row.student_name,
            &row.total_test_cases.to_string(),
            &row.passed_test_cases.to_string(),
            &row.percentage_marks.to_string(),
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

    writer.write_record([
        "student_name",
        "section",
        "lab_number",
        "submission_time",
        "zip_filename",
        "status",
        "score",
        "max_score",
        "percentage",
        "passed_tests",
        "total_tests",
        "execution_time_total_ms",
        "notes",
    ])?;

    for row in rows {
        writer.write_record(&[
            &row.student_name,
            row.section.as_deref().unwrap_or("Default"),
            &row.lab_number,
            &row.submission_time,
            &row.zip_filename,
            &row.status,
            &format!("{:.2}", row.score),
            &format!("{:.2}", row.max_score),
            &format!("{:.2}", row.percentage),
            &row.passed_tests.to_string(),
            &row.total_tests.to_string(),
            &row.execution_time_total_ms.to_string(),
            &row.notes,
        ])?;
    }

    writer.flush()?;

    // Also write a copy to output_dir/master_report.csv if available
    let root_csv = output_dir.join("master_report.csv");
    if let Ok((root_file, _)) = open_csv_resiliently(output_dir, &root_csv) {
        let mut root_writer = csv::Writer::from_writer(root_file);
        let _ = root_writer.write_record([
            "student_name",
            "section",
            "lab_number",
            "submission_time",
            "zip_filename",
            "status",
            "score",
            "max_score",
            "percentage",
            "passed_tests",
            "total_tests",
            "execution_time_total_ms",
            "notes",
        ]);
        for row in rows {
            let _ = root_writer.write_record(&[
                &row.student_name,
                row.section.as_deref().unwrap_or("Default"),
                &row.lab_number,
                &row.submission_time,
                &row.zip_filename,
                &row.status,
                &format!("{:.2}", row.score),
                &format!("{:.2}", row.max_score),
                &format!("{:.2}", row.percentage),
                &row.passed_tests.to_string(),
                &row.total_tests.to_string(),
                &row.execution_time_total_ms.to_string(),
                &row.notes,
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
