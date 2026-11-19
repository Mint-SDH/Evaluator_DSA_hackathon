use crate::platform;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

const COMPILATION_TIMEOUT_SECS: u64 = 15;

static ACTIVE_MAKE_TOOL: OnceLock<Option<&'static str>> = OnceLock::new();

/// Discovers the active make binary on the system once, caching it for all workers.
pub fn get_active_make_binary() -> Option<&'static str> {
    *ACTIVE_MAKE_TOOL.get_or_init(|| {
        #[cfg(windows)]
        let candidates = ["mingw32-make", "make"];
        #[cfg(not(windows))]
        let candidates = ["make"];

        for &cand in &candidates {
            let mut cmd = Command::new(cand);
            cmd.arg("--version");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            if let Ok(output) = cmd.output() {
                if output.status.success() {
                    return Some(cand);
                }
            }
        }
        None
    })
}

/// Compiles student C code in a shell-free, cross-platform manner.
/// Stage 1 (Primary): Locate and invoke Makefile (via mingw32-make or make) in work_dir.
/// Stage 2 (Fallback): Direct GCC invocation if Makefile is missing or make fails.
pub fn compile_submission(work_dir: &Path) -> Result<PathBuf, String> {
    // 0. Clean any pre-existing / stale binaries and object files from student's zip
    clean_stale_build_artifacts(work_dir);

    // -------------------------------------------------------------
    // STAGE 1: Makefile Execution (Primary)
    // -------------------------------------------------------------
    let has_makefile = ["makefile", "Makefile", "GNUmakefile"]
        .iter()
        .any(|name| work_dir.join(name).is_file());

    let mut stage1_make_error = String::new();

    if has_makefile {
        if let Some(make_bin) = get_active_make_binary() {
            let mut make_cmd = Command::new(make_bin);
            // Unconditionally make all targets (-B) to prevent stale bundled binaries from skipping compilation
            make_cmd.arg("-B");
            make_cmd.current_dir(work_dir);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                make_cmd.creation_flags(CREATE_NO_WINDOW);
            }
            make_cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

            let mut make_succeeded = false;
            match make_cmd.spawn() {
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
                            let _ = child.wait();

                            if status.success() {
                                make_succeeded = true;
                            } else {
                                stage1_make_error = format!(
                                    "'{make_bin}' exited with status {status}.\nStderr:\n{stderr}\nStdout:\n{stdout}"
                                );
                            }
                        }
                        Ok(None) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            stage1_make_error = format!(
                                "'{make_bin}' timed out after {COMPILATION_TIMEOUT_SECS} seconds."
                            );
                        }
                        Err(e) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            stage1_make_error = format!("Error waiting on '{make_bin}': {e}");
                        }
                    }
                }
                Err(e) => {
                    stage1_make_error = format!("Could not spawn '{make_bin}': {e}");
                }
            }

            if make_succeeded {
                if let Some(bin) = find_executable_in_dir(work_dir) {
                    return Ok(bin);
                } else {
                    stage1_make_error = format!(
                        "Makefile executed successfully but no executable binary was detected in {}",
                        work_dir.display()
                    );
                }
            }
        } else {
            stage1_make_error = "No suitable make utility (mingw32-make or make) found in system PATH.".to_string();
        }
    } else {
        stage1_make_error = "No Makefile found in the working directory.".to_string();
    }

    // -------------------------------------------------------------
    // STAGE 2: Direct GCC Invocation (Fallback)
    // -------------------------------------------------------------
    let output_bin_name = platform::executable_name("student_bin");
    let output_bin_path = work_dir.join(&output_bin_name);

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

    let stage2_gcc_error: String;

    if c_files.is_empty() {
        stage2_gcc_error = "No .c source files discovered in the submission directory.".to_string();
    } else {
        let mut gcc_cmd = Command::new("gcc");
        gcc_cmd.current_dir(work_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            gcc_cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Use -O0 for ultra-fast compilation (3x faster than -O2)
        gcc_cmd.arg("-O0");

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
                        let _ = child.wait();

                        if status.success() && output_bin_path.exists() {
                            return Ok(output_bin_path);
                        } else {
                            stage2_gcc_error = format!(
                                "GCC exited with status {status}.\nStderr:\n{stderr}\nStdout:\n{stdout}"
                            );
                        }
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        stage2_gcc_error = format!(
                            "Direct GCC compilation timed out after {COMPILATION_TIMEOUT_SECS} seconds."
                        );
                    }
                    Err(e) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        stage2_gcc_error = format!("Failed waiting on GCC process: {e}");
                    }
                }
            }
            Err(e) => {
                stage2_gcc_error = format!(
                    "Failed to execute 'gcc'. Ensure GCC is installed and on the system PATH. Error: {e}"
                );
            }
        }
    }

    Err(format!(
        "Compilation Failed:\n--- Stage 1 (Makefile) ---\n{stage1_make_error}\n--- Stage 2 (Direct GCC) ---\n{stage2_gcc_error}"
    ))
}

/// Cleans any pre-existing binaries or object files that were bundled into the submission archive.
fn clean_stale_build_artifacts(dir: &Path) {
    let stale_target_names = ["wordindex", "client", "a", "main", "run", "output", "index", "student_bin"];
    for entry in WalkDir::new(dir).max_depth(3).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            let mut should_delete = false;
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let ext_lower = ext.to_ascii_lowercase();
                if ext_lower == "o" || ext_lower == "obj" || ext_lower == "exe" || ext_lower == "out" {
                    should_delete = true;
                }
            } else if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
                let lower = file_name.to_ascii_lowercase();
                if stale_target_names.contains(&lower.as_str()) {
                    should_delete = true;
                }
            }
            if should_delete {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Discovers an executable binary produced in the working directory.
fn find_executable_in_dir(dir: &Path) -> Option<PathBuf> {
    // Check known standard names first (wordindex, client, a, main, etc.)
    let standard_names = [
        platform::executable_name("wordindex"),
        platform::executable_name("client"),
        platform::executable_name("a"),
        platform::executable_name("main"),
        platform::executable_name("output"),
        platform::executable_name("run"),
        platform::executable_name("index"),
        platform::executable_name("student_bin"),
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
                        if (mode & 0o111) != 0 {
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
