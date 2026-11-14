use crate::platform;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

const COMPILATION_TIMEOUT_SECS: u64 = 15;

/// Compiles student C code in a shell-free, cross-platform manner.
/// Stage 1: Primary direct GCC invocation via Rust Command.
/// Stage 2: Fallback to make / mingw32-make if a Makefile is present.
pub fn compile_submission(work_dir: &Path) -> Result<PathBuf, String> {
    let output_bin_name = platform::executable_name("student_bin");
    let output_bin_path = work_dir.join(&output_bin_name);

    // If an old binary exists from a previous step, remove it
    if output_bin_path.exists() {
        let _ = std::fs::remove_file(&output_bin_path);
    }

    // -------------------------------------------------------------
    // STAGE 1: Direct GCC invocation (Primary)
    // -------------------------------------------------------------
    let mut c_files: Vec<PathBuf> = Vec::new();
    let mut header_dirs: HashSet<PathBuf> = HashSet::new();

    header_dirs.insert(work_dir.to_path_buf());

    for entry in WalkDir::new(work_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let ext_lower = ext.to_ascii_lowercase();
                if ext_lower == "c" {
                    c_files.push(path.to_path_buf());
                    if let Some(parent) = path.parent() {
                        header_dirs.insert(parent.to_path_buf());
                    }
                } else if ext_lower == "h" {
                    if let Some(parent) = path.parent() {
                        header_dirs.insert(parent.to_path_buf());
                    }
                }
            }
        }
    }

    let stage1_error: String;

    if c_files.is_empty() {
        stage1_error = "No .c source files discovered in the submission directory.".to_string();
    } else {
        let mut gcc_cmd = Command::new("gcc");
        gcc_cmd.current_dir(work_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            gcc_cmd.creation_flags(CREATE_NO_WINDOW);
        }
        gcc_cmd.arg("-O2");

        for c_file in &c_files {
            gcc_cmd.arg(c_file);
        }

        for h_dir in &header_dirs {
            gcc_cmd.arg(format!("-I{}", h_dir.display()));
        }

        gcc_cmd.arg("-o").arg(&output_bin_path);
        gcc_cmd.arg("-lm");

        gcc_cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        match gcc_cmd.spawn() {
            Ok(mut child) => {
                match child.wait_timeout(Duration::from_secs(COMPILATION_TIMEOUT_SECS)) {
                    Ok(Some(status)) => {
                        let mut stderr = String::new();
                        if let Some(mut err_pipe) = child.stderr.take() {
                            let _ = err_pipe.read_to_string(&mut stderr);
                        }
                        let mut stdout = String::new();
                        if let Some(mut out_pipe) = child.stdout.take() {
                            let _ = out_pipe.read_to_string(&mut stdout);
                        }

                        if status.success() && output_bin_path.exists() {
                            return Ok(output_bin_path);
                        } else {
                            stage1_error = format!(
                                "GCC exited with status {status}.\nStderr:\n{stderr}\nStdout:\n{stdout}"
                            );
                        }
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        stage1_error = format!(
                            "Direct GCC compilation timed out after {COMPILATION_TIMEOUT_SECS} seconds."
                        );
                    }
                    Err(e) => {
                        stage1_error = format!("Failed waiting on GCC process: {e}");
                    }
                }
            }
            Err(e) => {
                stage1_error = format!(
                    "Failed to execute 'gcc'. Ensure GCC is installed and on the system PATH. Error: {e}"
                );
            }
        }
    }

    // -------------------------------------------------------------
    // STAGE 2: Makefile Fallback
    // -------------------------------------------------------------
    let has_makefile = ["makefile", "Makefile", "GNUmakefile"]
        .iter()
        .any(|name| work_dir.join(name).exists());

    let mut stage2_error = String::new();

    if has_makefile {
        #[cfg(windows)]
        let make_candidates = ["mingw32-make", "make"];
        #[cfg(not(windows))]
        let make_candidates = ["make"];

        let mut make_succeeded = false;

        for &make_bin in &make_candidates {
            let mut make_cmd = Command::new(make_bin);
            make_cmd.current_dir(work_dir);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                make_cmd.creation_flags(CREATE_NO_WINDOW);
            }
            make_cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

            match make_cmd.spawn() {
                Ok(mut child) => {
                    match child.wait_timeout(Duration::from_secs(COMPILATION_TIMEOUT_SECS)) {
                        Ok(Some(status)) => {
                            let mut stderr = String::new();
                            if let Some(mut err_pipe) = child.stderr.take() {
                                let _ = err_pipe.read_to_string(&mut stderr);
                            }

                            if status.success() {
                                make_succeeded = true;
                                break;
                            } else {
                                stage2_error = format!(
                                    "'{make_bin}' failed with status {status}.\nStderr: {stderr}"
                                );
                            }
                        }
                        Ok(None) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            stage2_error = format!(
                                "'{make_bin}' timed out after {COMPILATION_TIMEOUT_SECS} seconds."
                            );
                        }
                        Err(e) => {
                            stage2_error = format!("Error waiting on '{make_bin}': {e}");
                        }
                    }
                }
                Err(e) => {
                    stage2_error = format!("Could not spawn '{make_bin}': {e}");
                }
            }
        }

        if make_succeeded {
            // Check for resulting executable
            if let Some(bin) = find_executable_in_dir(work_dir) {
                return Ok(bin);
            } else {
                stage2_error = format!(
                    "Makefile executed successfully but no executable binary was detected in {}",
                    work_dir.display()
                );
            }
        }
    } else {
        stage2_error = "No Makefile found in working directory.".to_string();
    }

    Err(format!(
        "Compilation Failed:\n--- Stage 1 (Direct GCC) ---\n{stage1_error}\n--- Stage 2 (Makefile) ---\n{stage2_error}"
    ))
}

/// Discovers an executable binary produced in the working directory.
fn find_executable_in_dir(dir: &Path) -> Option<PathBuf> {
    // Check known standard names first
    let standard_names = [
        platform::executable_name("student_bin"),
        platform::executable_name("client"),
        platform::executable_name("a"),
        platform::executable_name("run"),
        platform::executable_name("main"),
    ];

    for name in &standard_names {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }

    // Inspect files in directory for executable characteristics
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                #[cfg(windows)]
                {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if ext.eq_ignore_ascii_case("exe") {
                            return Some(path);
                        }
                    }
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = path.metadata() {
                        let mode = meta.permissions().mode();
                        // Check if owner, group, or other execute bits are set
                        if (mode & 0o111) != 0 {
                            // Ignore shell scripts or text files
                            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                                if ext == "sh" || ext == "py" || ext == "pl" {
                                    continue;
                                }
                            }
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    None
}
