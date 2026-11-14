use crate::models::{IndividualReport, MasterSummaryRow};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes the detailed JSON report for a single student submission.
pub fn write_student_report(
    output_dir: &Path,
    report: &IndividualReport,
    zip_stem: &str,
) -> io::Result<PathBuf> {
    let reports_dir = output_dir.join("student_reports");
    if !reports_dir.exists() {
        std::fs::create_dir_all(&reports_dir)?;
    }

    let report_filename = format!("{zip_stem}_report.json");
    let report_path = reports_dir.join(report_filename);

    let json_content = serde_json::to_string_pretty(report)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut file = File::create(&report_path)?;
    file.write_all(json_content.as_bytes())?;
    file.flush()?;

    Ok(report_path)
}

/// Writes the master summary CSV aggregating results for all submissions.
/// If master_report.csv is locked by an external application (such as Excel or an editor),
/// retries with backoff and falls back to a timestamped file so results are never lost.
pub fn write_master_csv(
    output_dir: &Path,
    rows: &[MasterSummaryRow],
) -> io::Result<PathBuf> {
    if !output_dir.exists() {
        std::fs::create_dir_all(output_dir)?;
    }

    let default_csv_path = output_dir.join("master_report.csv");
    let (file, target_path) = open_csv_resiliently(output_dir, &default_csv_path)?;

    let mut writer = csv::Writer::from_writer(file);

    // Write header
    writer.write_record([
        "student_name",
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

    // Write all rows
    for row in rows {
        writer.write_record(&[
            &row.student_name,
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

    // If still locked (e.g. open in Excel or another program), save to timestamped fallback file
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let fallback_name = format!("master_report_{timestamp}.csv");
    let fallback_path = output_dir.join(&fallback_name);
    eprintln!(
        "\nNotice: '{}' is currently locked by another application (e.g., Excel/editor).\nSaving master summary to '{}' instead.\n",
        target.display(),
        fallback_path.display()
    );
    let f = File::create(&fallback_path)?;
    Ok((f, fallback_path))
}
