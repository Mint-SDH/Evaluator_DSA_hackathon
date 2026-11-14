use crate::models::StudentMetadata;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use walkdir::WalkDir;
use zip::ZipArchive;

pub struct ExtractedSubmission {
    pub temp_dir: TempDir,
    pub work_dir: PathBuf,
    pub metadata: StudentMetadata,
}

/// Safely extracts a student's submission into an ephemeral directory within the sandbox.
/// The original .zip archive is opened strictly in read-only mode and is never modified or deleted.
pub fn extract_submission(
    zip_path: &Path,
    sandbox_dir: &Path,
    resolved_base_lab: Option<&str>,
) -> io::Result<ExtractedSubmission> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;

    let filename = zip_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown.zip".to_string());

    let stem = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // 1. Extract metadata with strict 4-tier lab_number resolution priority:
    // Priority 1: Explicit CLI Override (passed via resolved_base_lab)
    // Priority 2: testcases.json field (passed via resolved_base_lab)
    // Priority 3: Filename parsing fallback (Lab<N> token in filename)
    // Priority 4: Final fallback "Unknown_Lab"
    let (student_name, detected_lab) = parse_name_and_lab(&stem);
    let lab_number = resolved_base_lab
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| detected_lab.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "Unknown_Lab".to_string());

    let submission_time = extract_submission_timestamp(&mut archive, zip_path);

    let metadata = StudentMetadata {
        lab_number,
        student_name,
        submission_time,
        zip_filename: filename,
    };

    // 2. Create isolated ephemeral directory in sandbox
    let temp_dir = tempfile::Builder::new()
        .prefix("sub_")
        .tempdir_in(sandbox_dir)?;

    let target_root = temp_dir.path();

    // 3. Safely extract files, sanitizing paths against directory traversal (Zip Slip)
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;

        // Enclosed name prevents ../../ traversal outside target_root
        let enclosed = match file.enclosed_name() {
            Some(path) => path.to_owned(),
            None => continue,
        };

        // Skip macOS metadata folders and junk files
        let path_str = enclosed.to_string_lossy();
        if path_str.starts_with("__MACOSX")
            || path_str.contains("/__MACOSX")
            || path_str.contains("\\__MACOSX")
            || path_str.ends_with(".DS_Store")
            || path_str.starts_with("._")
            || path_str.contains("/._")
            || path_str.contains("\\._")
        {
            continue;
        }

        let out_path = target_root.join(&enclosed);

        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let mut outfile = File::create(&out_path)?;
            io::copy(&mut file, &mut outfile)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
                }
            }
        }
    }

    // 4. Locate the working directory containing C source files
    let work_dir = locate_c_work_dir(target_root);

    Ok(ExtractedSubmission {
        temp_dir,
        work_dir,
        metadata,
    })
}

/// Resolves the lab number following the strict 4-tier priority order:
/// 1. Priority 1 (Explicit CLI Override): CLI `--lab <VALUE>` if non-empty
/// 2. Priority 2 (testcases.json Field): `test_suite.lab_number` if non-empty
/// 3. Priority 3 (Filename Parsing Fallback): `Lab<N>` token from zip stem
/// 4. Priority 4 (Final Fallback): "Unknown_Lab"
pub fn resolve_lab_number(
    cli_lab: Option<&str>,
    json_lab: Option<&str>,
    zip_stem: &str,
) -> String {
    // Priority 1: Explicit CLI Override
    if let Some(lab) = cli_lab {
        let trimmed = lab.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Priority 2: testcases.json Field (Primary Automatic Source)
    if let Some(lab) = json_lab {
        let trimmed = lab.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Priority 3: Filename Parsing Fallback (e.g. Lab1 / Lab2 in stem)
    let (_, detected_lab) = parse_name_and_lab(zip_stem);
    if let Some(lab) = detected_lab {
        let trimmed = lab.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Priority 4: Final Fallback
    "Unknown_Lab".to_string()
}

/// Parses the student name and lab identifier from the submission zip filename stem.
/// Example: PES2UG23CS001_StudentA_Lab1_Sub1 -> (PES2UG23CS001_StudentA, Some("Lab1"))
pub fn parse_name_and_lab(stem: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = stem.split('_').collect();
    let mut name_parts = Vec::new();
    let mut detected_lab = None;

    for part in parts {
        let lower = part.to_ascii_lowercase();
        let trimmed_digits = lower.trim_start_matches("lab").trim_start_matches(['-', '_']);
        if lower.starts_with("lab") && !trimmed_digits.is_empty() && trimmed_digits.chars().all(|c| c.is_ascii_digit()) {
            detected_lab = Some(part.to_string());
        } else if lower.starts_with("sub") || lower.starts_with("attempt") {
            // Skip submission attempt suffix from the student name
            continue;
        } else {
            name_parts.push(part);
        }
    }

    let student_name = if name_parts.is_empty() {
        stem.to_string()
    } else {
        name_parts.join("_")
    };

    (student_name, detected_lab)
}

/// Extracts the submission time by inspecting the most recent internal file modification timestamp
/// in the zip archive, falling back to the filesystem modification time if unavailable.
fn extract_submission_timestamp<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    zip_path: &Path,
) -> String {
    let mut latest_time: Option<(u16, u8, u8, u8, u8, u8)> = None;

    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if let Some(dt) = entry.last_modified() {
                let current = (
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                );
                match latest_time {
                    None => latest_time = Some(current),
                    Some(existing) if current > existing => latest_time = Some(current),
                    _ => {}
                }
            }
        }
    }

    if let Some((year, month, day, hour, min, _sec)) = latest_time {
        format!("{day:02}-{month:02}-{year:04} {hour:02}:{min:02}")
    } else if let Ok(meta) = zip_path.metadata() {
        if let Ok(modified) = meta.modified() {
            let dt: chrono::DateTime<chrono::Local> = modified.into();
            dt.format("%d-%m-%Y %H:%M").to_string()
        } else {
            chrono::Local::now().format("%d-%m-%Y %H:%M").to_string()
        }
    } else {
        chrono::Local::now().format("%d-%m-%Y %H:%M").to_string()
    }
}

/// Searches the extracted directory hierarchy to locate the best directory to compile:
/// 1. Directory containing client.c (primary driver)
/// 2. Directory containing main.c
/// 3. Directory with the greatest count of .c source files
/// 4. Root extracted directory as fallback
fn locate_c_work_dir(root: &Path) -> PathBuf {
    let mut dir_c_counts: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    let mut client_c_dir = None;
    let mut main_c_dir = None;

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = file_name.to_ascii_lowercase();
                if lower == "client.c" {
                    if let Some(parent) = path.parent() {
                        client_c_dir = Some(parent.to_path_buf());
                        break;
                    }
                } else if lower == "main.c" && main_c_dir.is_none() {
                    if let Some(parent) = path.parent() {
                        main_c_dir = Some(parent.to_path_buf());
                    }
                }

                if lower.ends_with(".c") {
                    if let Some(parent) = path.parent() {
                        *dir_c_counts.entry(parent.to_path_buf()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    if let Some(dir) = client_c_dir {
        return dir;
    }

    if let Some(dir) = main_c_dir {
        return dir;
    }

    if let Some((best_dir, _count)) = dir_c_counts.into_iter().max_by_key(|&(_, count)| count) {
        return best_dir;
    }

    root.to_path_buf()
}
