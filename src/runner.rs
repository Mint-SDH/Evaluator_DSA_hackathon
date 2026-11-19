use crate::models::{TestCase, TestCaseResult, TestSuite, Verdict};
use crate::platform;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024; // 2 MB safety cap
const DEFAULT_TIMEOUT_MS: u64 = 2000;

static POINTLESS_PRINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(debug|enter|input|reading|opening|open|file|loading|load|init|trie|total|done|finished|finish|end|result:|results:|time|elapsed|press any key|---|===|\*\*\*|>>>|inserting|searching|starting|success|error:|warning:|word count|program executed|testing|allocat)\b")
        .expect("Valid static regex for pointless prints")
});

static WORD_COUNT_FORMAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*word\s*:\s*(\S+)\s*count\s*:\s*(\d+)\s*$")
        .expect("Valid static regex for word count")
});

static WORD_LINES_FORMAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*word\s*:\s*(\S+)\s*:\s*([\d\s]+)\s*$")
        .expect("Valid static regex for word line index")
});

static ANSWER_SIGNATURE_COLON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:word\s*[:=]?\s*)?[a-z0-9_]+\s*(?:count\s*[:=]|[:=])\s*[\d\s]+")
        .expect("Valid static regex for answer signature")
});

static EXPECTED_REGEX_CACHE: LazyLock<RwLock<HashMap<String, Arc<Vec<Regex>>>>> = LazyLock::new(|| {
    RwLock::new(HashMap::new())
});

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

/// Builds a flexible regular expression pattern from an expected line.
/// Standardizes:
/// - Word Frequency format: "word : <word> count : <count>"
///   Allows variations: "word: better count: 2", "word:better:2", "better count : 2", "better : 2", etc.
/// - Word Line Index format: "word : <word> : <lines...>"
/// - Delimiters (colons, equals, commas) -> \s*[:=,]\s*
/// - Spacing between alphanumeric tokens -> \s+
/// - Regex metacharacters in literal tokens are safely escaped.
pub fn build_line_regex_pattern(expected_line: &str) -> String {
    let trimmed = expected_line.trim();
    if trimmed.is_empty() {
        return r"^\s*$".to_string();
    }

    // Pattern A: Word Frequency format: "word : <word> count : <count>"
    if let Some(caps) = WORD_COUNT_FORMAT_RE.captures(trimmed) {
        let word = regex::escape(&caps[1]);
        let count = regex::escape(&caps[2]);
        return format!(
            r"^\s*(?:word\s*[:=]?\s*)?\b{}\b(?:[,\s]*(?:count\b)?\s*[:=]?\s*|\s+count\s*[:=]?\s*|\s*[:=]\s*){}\s*$",
            word, count
        );
    }

    // Pattern B: Word Line Index format: "word : <word> : <line numbers...>"
    if let Some(caps) = WORD_LINES_FORMAT_RE.captures(trimmed) {
        let word = regex::escape(&caps[1]);
        let lines_str = &caps[2];
        let nums: Vec<&str> = lines_str.split_whitespace().collect();
        let escaped_nums: Vec<String> = nums.iter().map(|n| regex::escape(n)).collect();
        let nums_pattern = escaped_nums.join(r"\s+");
        return format!(
            r"^\s*(?:word\s*[:=]?\s*)?\b{}\b(?:[,\s]*(?:lines?\b)?\s*[:=]?\s*|\s+lines?\s*[:=]?\s*|\s*[:=]\s*){}\s*$",
            word, nums_pattern
        );
    }

    // Pattern C: General Colon-separated format: "<part1> : <part2> ..."
    if trimmed.contains(':') {
        let parts: Vec<&str> = trimmed.split(':').collect();
        let escaped_parts: Vec<String> = parts
            .iter()
            .map(|p| {
                let tokens: Vec<&str> = p.split_whitespace().collect();
                let escaped_tokens: Vec<String> = tokens.iter().map(|t| regex::escape(t)).collect();
                escaped_tokens.join(r"\s+")
            })
            .collect();
        let body = escaped_parts.join(r"\s*:\s*");
        return format!(r"^\s*{}\s*$", body);
    }

    // Pattern D: General Equals-separated format: "<part1> = <part2>"
    if trimmed.contains('=') {
        let parts: Vec<&str> = trimmed.split('=').collect();
        let escaped_parts: Vec<String> = parts
            .iter()
            .map(|p| {
                let tokens: Vec<&str> = p.split_whitespace().collect();
                let escaped_tokens: Vec<String> = tokens.iter().map(|t| regex::escape(t)).collect();
                escaped_tokens.join(r"\s+")
            })
            .collect();
        let body = escaped_parts.join(r"\s*=\s*");
        return format!(r"^\s*{}\s*$", body);
    }

    // Pattern E: Whitespace-separated tokens
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let escaped_tokens: Vec<String> = tokens.iter().map(|t| regex::escape(t)).collect();
    let body = escaped_tokens.join(r"\s+");
    format!(r"^\s*{}\s*$", body)
}

/// Builds a compiled case-insensitive Regex from an expected line.
pub fn build_line_regex(expected_line: &str) -> Result<Regex, regex::Error> {
    let pattern = build_line_regex_pattern(expected_line);
    RegexBuilder::new(&pattern).case_insensitive(true).build()
}

/// Checks whether a line matches common student prompt, debug, or banner statements using regex.
pub fn is_pointless_print_statement(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    POINTLESS_PRINT_RE.is_match(trimmed)
}

/// Retrieves or compiles the expected line regexes for a given expected output string.
/// Thread-safe and zero overhead on cache hits across hundreds of student evaluations.
fn get_or_compile_expected_regexes(expected: &str, expected_non_empty: &[&str]) -> Option<Arc<Vec<Regex>>> {
    if let Ok(cache) = EXPECTED_REGEX_CACHE.read() {
        if let Some(cached) = cache.get(expected) {
            return Some(Arc::clone(cached));
        }
    }

    let mut compiled = Vec::with_capacity(expected_non_empty.len());
    for exp_line in expected_non_empty {
        match build_line_regex(exp_line) {
            Ok(re) => compiled.push(re),
            Err(_) => return None,
        }
    }

    let arc = Arc::new(compiled);
    if let Ok(mut cache) = EXPECTED_REGEX_CACHE.write() {
        cache.insert(expected.to_string(), Arc::clone(&arc));
    }
    Some(arc)
}

/// Evaluates student stdout against expected stdout using regular expressions.
/// Supports:
/// 1. Custom regex pattern if `custom_regex` is provided in the test case.
/// 2. Strict exact match after CRLF and trailing whitespace normalization.
/// 3. Dynamic line-by-line regex matching:
///    - Tolerates colon spacing (e.g. \s*:\s*)
///    - Tolerates multiple spaces/tabs (e.g. \s+)
///    - Case-insensitivity ((?i))
///    - Automatically ignores pointless / debug print statements
///    - Ensures genuinely wrong answers (wrong numbers, missing lines, extra wrong entries) fail.
pub fn evaluate_with_regex(
    actual: &str,
    expected: &str,
    custom_regex: Option<&str>,
    weight: f64,
) -> (Verdict, bool, f64, Option<String>) {
    // 1. Check custom regex pattern if provided
    if let Some(custom_pattern) = custom_regex {
        let pattern_trimmed = custom_pattern.trim();
        if !pattern_trimmed.is_empty() {
            match RegexBuilder::new(pattern_trimmed)
                .multi_line(true)
                .dot_matches_new_line(true)
                .case_insensitive(true)
                .build()
            {
                Ok(re) => {
                    if re.is_match(actual) {
                        return (
                            Verdict::Accepted,
                            true,
                            weight,
                            Some("Passed (Matched custom regex pattern)".to_string()),
                        );
                    } else {
                        return (Verdict::WrongAnswer, false, 0.0, None);
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Invalid custom regex pattern: {e}");
                }
            }
        }
    }

    // 2. Strict equality match (CRLF and trailing line spaces normalized)
    let norm_actual = normalize_output(actual);
    let norm_expected = normalize_output(expected);
    if norm_actual == norm_expected {
        return (Verdict::Accepted, true, weight, None);
    }

    // 3. Dynamic Line-by-Line Regex Matching
    let expected_non_empty: Vec<&str> = expected
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    if expected_non_empty.is_empty() {
        let actual_non_empty: Vec<&str> = actual
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        if actual_non_empty.is_empty() {
            return (Verdict::Accepted, true, weight, None);
        } else {
            return (Verdict::WrongAnswer, false, 0.0, None);
        }
    }

    // Retrieve or compile regexes for expected lines with zero-overhead cache
    let expected_regexes = match get_or_compile_expected_regexes(expected, &expected_non_empty) {
        Some(re_vec) => re_vec,
        None => return (Verdict::WrongAnswer, false, 0.0, None),
    };

    let actual_lines: Vec<&str> = actual.lines().collect();
    let actual_non_empty: Vec<&str> = actual_lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();

    // 3a. Direct 1-to-1 regex match across non-empty lines
    if actual_non_empty.len() == expected_regexes.len() {
        let mut all_matched = true;
        for (act, re) in actual_non_empty.iter().zip(expected_regexes.iter()) {
            if !re.is_match(act) {
                all_matched = false;
                break;
            }
        }
        if all_matched {
            return (
                Verdict::Accepted,
                true,
                weight,
                Some("Passed (Matched via regex: spacing/casing normalized)".to_string()),
            );
        }
    }

    // 3b. Regex matching with pointless print statement filtering
    // Sequential match of all expected regexes inside student output lines
    let mut exp_idx = 0;
    let mut matched_indices = Vec::new();

    for (act_idx, act_line) in actual_non_empty.iter().enumerate() {
        if exp_idx < expected_regexes.len() {
            if expected_regexes[exp_idx].is_match(act_line) {
                matched_indices.push(act_idx);
                exp_idx += 1;
            }
        }
    }

    if exp_idx == expected_regexes.len() {
        // All expected lines were found in order!
        // Now verify whether any un-matched lines are extra wrong answers vs harmless debug/prompt prints.
        let all_expected_have_colon = expected_non_empty.iter().all(|l| l.contains(':'));
        let answer_signature_regex = if all_expected_have_colon {
            Some(&*ANSWER_SIGNATURE_COLON_RE)
        } else {
            None
        };

        let mut has_unrecognized_answer = false;
        for (act_idx, act_line) in actual_non_empty.iter().enumerate() {
            if matched_indices.contains(&act_idx) {
                continue;
            }
            if let Some(sig_re) = answer_signature_regex {
                if sig_re.is_match(act_line) {
                    has_unrecognized_answer = true;
                    break;
                }
            } else if !is_pointless_print_statement(act_line) {
                has_unrecognized_answer = true;
                break;
            }
        }

        if !has_unrecognized_answer {
            return (
                Verdict::Accepted,
                true,
                weight,
                Some("Passed (Matched via regex: ignored extraneous debug prints)".to_string()),
            );
        }
    }

    // 4. Multi-line contiguous window regex match
    let exp_len = expected_regexes.len();
    if actual_non_empty.len() >= exp_len {
        let all_expected_have_colon = expected_non_empty.iter().all(|l| l.contains(':'));
        let answer_signature_regex = if all_expected_have_colon {
            Some(&*ANSWER_SIGNATURE_COLON_RE)
        } else {
            None
        };

        for (start_idx, window) in actual_non_empty.windows(exp_len).enumerate() {
            let mut window_matched = true;
            for (act, re) in window.iter().zip(expected_regexes.iter()) {
                if !re.is_match(act) {
                    window_matched = false;
                    break;
                }
            }
            if window_matched {
                // Verify that all lines outside this window are harmless header/footer prints
                let end_idx = start_idx + exp_len;
                let mut outside_lines_valid = true;
                for (idx, line) in actual_non_empty.iter().enumerate() {
                    if idx < start_idx || idx >= end_idx {
                        if let Some(sig_re) = answer_signature_regex {
                            if sig_re.is_match(line) {
                                outside_lines_valid = false;
                                break;
                            }
                        } else if !is_pointless_print_statement(line) {
                            outside_lines_valid = false;
                            break;
                        }
                    }
                }

                if outside_lines_valid {
                    return (
                        Verdict::Accepted,
                        true,
                        weight,
                        Some("Passed (Matched via regex: ignored extraneous header/footer prints)".to_string()),
                    );
                }
            }
        }
    }

    (Verdict::WrongAnswer, false, 0.0, None)
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

    // 3. Concurrent lightweight reader threads (64KB stack) to prevent pipe deadlocks (Windows 4 KB pipe buffer)
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_thread = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut buf = Vec::new();
            if let Some(pipe) = stdout_pipe {
                let _ = pipe.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
            }
            buf
        });

    let stderr_thread = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
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
        Ok(Some(status)) => {
            // CRITICAL ON WINDOWS: reap child handle immediately to release executable locks
            let _ = child.wait();
            (Some(status), false)
        }
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

    // Drain and collect outputs from reader threads safely
    let stdout_bytes = stdout_thread.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let stderr_bytes = stderr_thread.map(|h| h.join().unwrap_or_default()).unwrap_or_default();

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

    // 6. Normal exit: regex-based output comparison
    let (verdict, passed, score, match_note) = evaluate_with_regex(
        &actual_stdout,
        &test_case.expected_stdout,
        test_case.expected_regex.as_deref(),
        test_case.weight,
    );

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
        error_message: match_note,
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
