use crate::models::{TestCase, TestCaseResult, TestSuite, Verdict};
use crate::platform;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024; // 2 MB safety cap
const DEFAULT_TIMEOUT_MS: u64 = 2000;

/// Normalizes output strings across Windows (CRLF \r\n) and Linux (LF \n):
/// 1. Splits lines on any newline format (\r\n, \n, or \r).
/// 2. Trims trailing whitespace (spaces and tabs) from each individual line.
/// 3. Joins with standard LF (\n).
/// 4. Trims any trailing newlines at the end of the text.
pub fn normalize_output(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end_matches(['\r', ' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// Runs a single test case against the student executable binary.
pub fn run_test_case(
    bin_path: &Path,
    work_dir: &Path,
    test_case: &TestCase,
    time_limit_ms: u64,
    command_args: &[String],
) -> TestCaseResult {
    // 1. Stage input files into the student's working directory
    for (filename, content) in &test_case.input_files {
        let file_path = work_dir.join(filename);
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = File::create(&file_path) {
            let _ = f.write_all(content.as_bytes());
        }
    }

    let mut cmd = Command::new(bin_path);
    cmd.current_dir(work_dir);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    if !command_args.is_empty() {
        cmd.args(command_args);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start_time = Instant::now();

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestCaseResult {
                test_case_id: test_case.id.clone(),
                test_case_name: test_case.name.clone(),
                verdict: Verdict::RuntimeError(format!("Failed to spawn process: {e}")).to_string(),
                passed: false,
                score: 0.0,
                max_score: test_case.weight,
                execution_time_ms: 0,
                actual_stdout: String::new(),
                expected_stdout: test_case.expected_stdout.clone(),
                stderr: format!("Spawn error: {e}"),
                error_message: Some(format!("Failed to spawn executable: {e}")),
            };
        }
    };

    // 2. Write stdin in a scoped block and drop ChildStdin immediately to send EOF
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(ref input_str) = test_case.stdin {
            let _ = stdin.write_all(input_str.as_bytes());
            let _ = stdin.flush();
        }
        // Explicit drop sends EOF to scanf / fgets / getchar
        drop(stdin);
    }

    // 3. Concurrent reader threads to prevent pipe deadlocks (Windows 4 KB pipe buffer)
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe {
            let _ = pipe.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
        }
        buf
    });

    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe {
            let _ = pipe.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
        }
        buf
    });

    // 4. Timeout watchdog
    let timeout = Duration::from_millis(time_limit_ms);
    let wait_res = child.wait_timeout(timeout);

    let (status_opt, timed_out) = match wait_res {
        Ok(Some(status)) => (Some(status), false),
        Ok(None) => {
            // Timed out: kill child process
            let _ = child.kill();
            // CRITICAL ON WINDOWS: child.wait() must be called to release process and file handles!
            let _ = child.wait();
            (None, true)
        }
        Err(_e) => {
            let _ = child.kill();
            let _ = child.wait();
            (None, false)
        }
    };

    let execution_time_ms = start_time.elapsed().as_millis();

    // Drain and collect outputs from reader threads
    let stdout_bytes = stdout_thread.join().unwrap_or_default();
    let stderr_bytes = stderr_thread.join().unwrap_or_default();

    let actual_stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let actual_stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    if timed_out {
        return TestCaseResult {
            test_case_id: test_case.id.clone(),
            test_case_name: test_case.name.clone(),
            verdict: Verdict::TimeLimitExceeded.to_string(),
            passed: false,
            score: 0.0,
            max_score: test_case.weight,
            execution_time_ms,
            actual_stdout,
            expected_stdout: test_case.expected_stdout.clone(),
            stderr: actual_stderr,
            error_message: Some(format!("Time Limit Exceeded ({time_limit_ms} ms)")),
        };
    }

    let status = match status_opt {
        Some(s) => s,
        None => {
            return TestCaseResult {
                test_case_id: test_case.id.clone(),
                test_case_name: test_case.name.clone(),
                verdict: Verdict::RuntimeError("Process terminated unexpectedly".to_string()).to_string(),
                passed: false,
                score: 0.0,
                max_score: test_case.weight,
                execution_time_ms,
                actual_stdout,
                expected_stdout: test_case.expected_stdout.clone(),
                stderr: actual_stderr,
                error_message: Some("Process terminated unexpectedly".to_string()),
            };
        }
    };

    // 5. Check for crash / runtime error (Segfault 0xC0000005, Stack Overflow 0xC00000FD, SIGSEGV, etc.)
    if let Some(error_desc) = platform::classify_exit_status(&status) {
        return TestCaseResult {
            test_case_id: test_case.id.clone(),
            test_case_name: test_case.name.clone(),
            verdict: Verdict::RuntimeError(error_desc.clone()).to_string(),
            passed: false,
            score: 0.0,
            max_score: test_case.weight,
            execution_time_ms,
            actual_stdout,
            expected_stdout: test_case.expected_stdout.clone(),
            stderr: actual_stderr,
            error_message: Some(error_desc),
        };
    }

    // 6. Normal exit: compare CRLF-normalized outputs
    let norm_actual = normalize_output(&actual_stdout);
    let norm_expected = normalize_output(&test_case.expected_stdout);

    let (verdict, passed, score) = if norm_actual == norm_expected {
        (Verdict::Accepted, true, test_case.weight)
    } else {
        (Verdict::WrongAnswer, false, 0.0)
    };

    TestCaseResult {
        test_case_id: test_case.id.clone(),
        test_case_name: test_case.name.clone(),
        verdict: verdict.to_string(),
        passed,
        score,
        max_score: test_case.weight,
        execution_time_ms,
        actual_stdout,
        expected_stdout: test_case.expected_stdout.clone(),
        stderr: actual_stderr,
        error_message: None,
    }
}

/// Evaluates all test cases in the test suite against the compiled student binary.
pub fn run_test_suite(
    bin_path: &Path,
    work_dir: &Path,
    test_suite: &TestSuite,
) -> (Vec<TestCaseResult>, f64, f64, usize, u128) {
    let time_limit_ms = test_suite.time_limit_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let command_args = test_suite.command_args.clone().unwrap_or_default();

    let mut results = Vec::with_capacity(test_suite.test_cases.len());
    let mut total_score = 0.0;
    let mut max_score = 0.0;
    let mut passed_count = 0;
    let mut total_time_ms = 0;

    for test_case in &test_suite.test_cases {
        let result = run_test_case(bin_path, work_dir, test_case, time_limit_ms, &command_args);
        total_score += result.score;
        max_score += result.max_score;
        if result.passed {
            passed_count += 1;
        }
        total_time_ms += result.execution_time_ms;
        results.push(result);
    }

    (results, total_score, max_score, passed_count, total_time_ms)
}
