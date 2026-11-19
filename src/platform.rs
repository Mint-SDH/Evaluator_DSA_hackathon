use std::path::Path;
use std::process::ExitStatus;
use std::thread::sleep;
use std::time::Duration;

/// Initializes platform-specific settings.
/// On Windows, disables system crash dialogs (WerFault) so segfaults/access violations (0xC0000005)
/// terminate immediately without hanging waiting for user interaction.
pub fn init_platform() {
    #[cfg(windows)]
    unsafe {
        const SEM_FAILCRITICALERRORS: u32 = 0x0001;
        const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
        const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
        extern "system" {
            fn SetErrorMode(uMode: u32) -> u32;
        }
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
}

/// Returns the platform-appropriate executable name.
/// On Windows, appends ".exe" (e.g. "student_bin.exe").
/// On Linux/Unix, returns the base name unchanged (e.g. "student_bin").
pub fn executable_name(base: &str) -> String {
    if std::env::consts::EXE_EXTENSION.is_empty() {
        base.to_string()
    } else {
        format!("{}.{}", base, std::env::consts::EXE_EXTENSION)
    }
}

/// Translates platform-specific termination status (Windows NTSTATUS vs Linux POSIX Signals)
/// into a unified human-readable error verdict if abnormal termination occurred.
/// Returns None if the process exited cleanly with code 0.
pub fn classify_exit_status(status: &ExitStatus) -> Option<String> {
    if status.success() {
        return None;
    }

    #[cfg(windows)]
    {
        if let Some(code) = status.code() {
            // Match against signed 32-bit representations of Windows NTSTATUS crash codes
            match code {
                // 0xC0000005: STATUS_ACCESS_VIOLATION / Segfault
                -1073741819 => Some("Runtime Error (Segmentation Fault / Access Violation [0xC0000005])".to_string()),
                // 0xC00000FD: STATUS_STACK_OVERFLOW
                -1073741571 => Some("Runtime Error (Stack Overflow [0xC00000FD])".to_string()),
                // 0xC0000094: STATUS_INTEGER_DIVIDE_BY_ZERO
                -1073741676 => Some("Runtime Error (Integer Division by Zero [0xC0000094])".to_string()),
                // 0xC000001D: STATUS_ILLEGAL_INSTRUCTION
                -1073741795 => Some("Runtime Error (Illegal Instruction [0xC000001D])".to_string()),
                // 0xC0000028: STATUS_BAD_STACK
                -1073741784 => Some("Runtime Error (Bad Stack [0xC0000028])".to_string()),
                // 0xC000008C: STATUS_ARRAY_BOUNDS_EXCEEDED
                -1073741684 => Some("Runtime Error (Array Bounds Exceeded [0xC000008C])".to_string()),
                // 0xC0000409: STATUS_STACK_BUFFER_OVERRUN
                -1073740791 => Some("Runtime Error (Stack Buffer Overrun [0xC0000409])".to_string()),
                _ => Some(format!("Runtime Error (Exit Code: {code})")),
            }
        } else {
            Some("Runtime Error (Process Terminated Abnormally)".to_string())
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(sig) = status.signal() {
            match sig {
                11 => Some("Runtime Error (Segmentation Fault [SIGSEGV 11])".to_string()),
                8 => Some("Runtime Error (Floating Point Exception / Division by Zero [SIGFPE 8])".to_string()),
                6 => Some("Runtime Error (Aborted [SIGABRT 6])".to_string()),
                7 => Some("Runtime Error (Bus Error [SIGBUS 7])".to_string()),
                9 => Some("Runtime Error (Killed [SIGKILL 9])".to_string()),
                4 => Some("Runtime Error (Illegal Instruction [SIGILL 4])".to_string()),
                _ => Some(format!("Runtime Error (Terminated by Signal: {sig})")),
            }
        } else if let Some(code) = status.code() {
            if code == 0 {
                None
            } else {
                Some(format!("Runtime Error (Exit Code: {code})"))
            }
        } else {
            Some("Runtime Error (Process Terminated Abnormally)".to_string())
        }
    }

    #[cfg(not(any(windows, unix)))]
    {
        if let Some(code) = status.code() {
            if code == 0 {
                None
            } else {
                Some(format!("Runtime Error (Exit Code: {code})"))
            }
        } else {
            Some("Runtime Error (Process Terminated Abnormally)".to_string())
        }
    }
}

/// Robustly removes a TempDir, mitigating Windows NTFS file-locking (OS Error 5 / 32)
/// that occurs when killed child processes hold binary handles for 10-50ms after termination.
pub fn robust_cleanup(temp_dir: tempfile::TempDir) -> std::io::Result<()> {
    let path = temp_dir.path().to_path_buf();

    // First attempt to close the TempDir normally
    let close_result = temp_dir.close();

    // Only if close() reported an error (e.g. temporary Windows file lock), run retry loop
    if close_result.is_err() {
        robust_remove_path(&path)?;
    }

    Ok(())
}

/// Robustly removes a directory path with a retry loop (5 retries with 30ms sleep).
/// Also handles read-only file attributes which can prevent directory deletion on Windows.
pub fn robust_remove_path<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(());
    }

    let mut last_err = None;
    for attempt in 1..=5 {
        // Attempt normal directory removal
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                // On Windows, if read-only attribute is set on extracted files, clear it
                #[cfg(windows)]
                {
                    clear_readonly_flags(path);
                }
                sleep(Duration::from_millis(30 * attempt));
            }
        }
    }

    // Final check: if the path is gone, return Ok
    if !path.exists() {
        Ok(())
    } else if let Some(e) = last_err {
        Err(e)
    } else {
        std::fs::remove_dir_all(path)
    }
}

#[cfg(windows)]
fn clear_readonly_flags(dir: &Path) {
    if let Ok(entries) = walkdir::WalkDir::new(dir).into_iter().collect::<Result<Vec<_>, _>>() {
        for entry in entries {
            if let Ok(metadata) = entry.metadata() {
                let mut permissions = metadata.permissions();
                if permissions.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    let _ = std::fs::set_permissions(entry.path(), permissions);
                }
            }
        }
    }
}
