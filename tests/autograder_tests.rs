use c_autograder::compiler;
use c_autograder::extractor;
use c_autograder::models::{TestCase, TestSuite};
use c_autograder::platform;
use c_autograder::reporter;
use c_autograder::runner;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_executable_name() {
    let name = platform::executable_name("test_bin");
    #[cfg(windows)]
    assert_eq!(name, "test_bin.exe");
    #[cfg(not(windows))]
    assert_eq!(name, "test_bin");
}

#[test]
fn test_lab_number_resolution_priority() {
    // Priority 1: CLI Override takes absolute precedence over JSON and filename
    let lab_p1 = extractor::resolve_lab_number(
        Some("LabOverride"),
        Some("Lab2"),
        "PES2UG23CS001_StudentA_Lab1_Sub1",
    );
    assert_eq!(lab_p1, "LabOverride");

    // Priority 2: testcases.json field takes precedence over filename when CLI is omitted
    let lab_p2 = extractor::resolve_lab_number(
        None,
        Some("Lab2"),
        "PES2UG23CS001_StudentA_Lab1_Sub1",
    );
    assert_eq!(lab_p2, "Lab2");

    // Priority 3: Filename parsing when both CLI and JSON are empty/omitted
    let lab_p3 = extractor::resolve_lab_number(
        None,
        None,
        "PES2UG23CS001_StudentA_Lab3_Sub1",
    );
    assert_eq!(lab_p3, "Lab3");

    // Priority 4: Final fallback "Unknown_Lab" when no source provides a lab
    let lab_p4 = extractor::resolve_lab_number(
        None,
        None,
        "PES2UG23CS001_StudentA",
    );
    assert_eq!(lab_p4, "Unknown_Lab");
}

#[test]
fn test_normalize_output() {
    let raw_windows = "Hello World  \r\nLine 2 \t\r\n\r\n";
    let raw_linux = "Hello World\nLine 2\n\n";

    let norm_win = runner::normalize_output(raw_windows);
    let norm_lin = runner::normalize_output(raw_linux);

    assert_eq!(norm_win, "Hello World\nLine 2");
    assert_eq!(norm_lin, "Hello World\nLine 2");
    assert_eq!(norm_win, norm_lin);
}

#[test]
fn test_full_pipeline_multi_file_c() {
    let temp_root = tempdir().expect("create temp dir");
    let sandbox_dir = temp_root.path().join(".temp_sandbox");
    let output_dir = temp_root.path().join("output");
    std::fs::create_dir_all(&sandbox_dir).unwrap();
    std::fs::create_dir_all(&output_dir).unwrap();

    // 1. Create a mock student zip with multi-file C code
    let zip_path = temp_root.path().join("PES2UG23CS001_StudentA_Lab1_Sub1.zip");
    {
        let file = File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        // client.c
        zip.start_file("nested/client.c", options).unwrap();
        zip.write_all(
            b"#include <stdio.h>\n#include \"calc.h\"\nint main() {\n    int a, b;\n    if (scanf(\"%d %d\", &a, &b) == 2) {\n        printf(\"SUM=%d\\n\", add(a, b));\n    }\n    return 0;\n}\n",
        )
        .unwrap();

        // calc.h
        zip.start_file("nested/calc.h", options).unwrap();
        zip.write_all(b"#ifndef CALC_H\n#define CALC_H\nint add(int a, int b);\n#endif\n")
            .unwrap();

        // calc.c
        zip.start_file("nested/calc.c", options).unwrap();
        zip.write_all(b"#include \"calc.h\"\nint add(int a, int b) {\n    return a + b;\n}\n")
            .unwrap();

        zip.finish().unwrap();
    }

    // 2. Extract submission
    let extracted = extractor::extract_submission(&zip_path, &sandbox_dir, Some("Lab1"))
        .expect("extract submission");

    assert_eq!(extracted.metadata.student_name, "PES2UG23CS001_StudentA");
    assert_eq!(extracted.metadata.lab_number, "Lab1");

    // 3. Compile submission
    let bin_path = compiler::compile_submission(&extracted.work_dir)
        .expect("compile student code");
    assert!(bin_path.exists());

    // 4. Create test suite
    let test_suite = TestSuite {
        problem_id: "AddNumbers".to_string(),
        lab_number: Some("Lab1".to_string()),
        time_limit_ms: Some(2000),
        command_args: None,
        test_cases: vec![
            TestCase {
                id: "1".to_string(),
                name: "Test Normal Addition".to_string(),
                stdin: Some("10 20\n".to_string()),
                input_files: HashMap::new(),
                expected_stdout: "SUM=30\n".to_string(),
                expected_regex: None,
                weight: 5.0,
            },
            TestCase {
                id: "2".to_string(),
                name: "Test Negative Addition".to_string(),
                stdin: Some("5 -10\n".to_string()),
                input_files: HashMap::new(),
                expected_stdout: "SUM=-5\n".to_string(),
                expected_regex: None,
                weight: 5.0,
            },
            TestCase {
                id: "3".to_string(),
                name: "Test Wrong Answer".to_string(),
                stdin: Some("2 2\n".to_string()),
                input_files: HashMap::new(),
                expected_stdout: "SUM=5\n".to_string(), // Expecting 5 while code emits 4
                expected_regex: None,
                weight: 5.0,
            },
        ],
    };

    // 5. Run tests
    let (results, total_score, max_score, passed_count, _) =
        runner::run_test_suite(&bin_path, &extracted.work_dir, &test_suite);

    assert_eq!(results.len(), 3);
    assert_eq!(passed_count, 2);
    assert_eq!(total_score, 10.0);
    assert_eq!(max_score, 15.0);
    assert_eq!(results[0].verdict, "Accepted");
    assert_eq!(results[1].verdict, "Accepted");
    assert_eq!(results[2].verdict, "Wrong Answer");

    // 6. Test robust cleanup (must remove folder cleanly)
    let extracted_path = extracted.temp_dir.path().to_path_buf();
    platform::robust_cleanup(extracted.temp_dir).expect("cleanup temp dir");
    assert!(!extracted_path.exists(), "Extracted folder must be wiped immediately");
}

#[test]
fn test_timeout_and_ntfs_cleanup() {
    let temp_root = tempdir().expect("create temp dir");
    let sandbox_dir = temp_root.path().join(".temp_sandbox");
    std::fs::create_dir_all(&sandbox_dir).unwrap();

    let zip_path = temp_root.path().join("PES2UG23CS002_TimeoutStudent_Lab1.zip");
    {
        let file = File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("client.c", options).unwrap();
        zip.write_all(b"#include <stdio.h>\nint main() {\n    volatile int x = 0;\n    while (1) { x++; }\n    return 0;\n}\n")
            .unwrap();
        zip.finish().unwrap();
    }

    let extracted = extractor::extract_submission(&zip_path, &sandbox_dir, Some("Lab1")).unwrap();
    let bin_path = compiler::compile_submission(&extracted.work_dir).unwrap();

    let test_suite = TestSuite {
        problem_id: "TimeoutTest".to_string(),
        lab_number: Some("Lab1".to_string()),
        time_limit_ms: Some(500), // 500ms timeout
        command_args: None,
        test_cases: vec![TestCase {
            id: "1".to_string(),
            name: "Infinite Loop Check".to_string(),
            stdin: None,
            input_files: HashMap::new(),
            expected_stdout: "Done".to_string(),
            expected_regex: None,
            weight: 10.0,
        }],
    };

    let (results, score, _, passed, _) =
        runner::run_test_suite(&bin_path, &extracted.work_dir, &test_suite);

    assert_eq!(passed, 0);
    assert_eq!(score, 0.0);
    assert_eq!(results[0].verdict, "Time Limit Exceeded");

    // Robust cleanup must successfully close process handles and delete binary
    let extracted_path = extracted.temp_dir.path().to_path_buf();
    platform::robust_cleanup(extracted.temp_dir).expect("robust cleanup after timeout");
    assert!(!extracted_path.exists(), "Timed-out process binary must be released and deleted");
}

#[test]
fn test_segfault_detection() {
    platform::init_platform();
    let temp_root = tempdir().expect("create temp dir");
    let sandbox_dir = temp_root.path().join(".temp_sandbox");
    std::fs::create_dir_all(&sandbox_dir).unwrap();

    let zip_path = temp_root.path().join("PES2UG23CS003_CrashStudent_Lab1.zip");
    {
        let file = File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("client.c", options).unwrap();
        zip.write_all(b"#include <stdio.h>\n#include <stdlib.h>\nint main() {\n    int *ptr = NULL;\n    *ptr = 42;\n    return 0;\n}\n")
            .unwrap();
        zip.finish().unwrap();
    }

    let extracted = extractor::extract_submission(&zip_path, &sandbox_dir, Some("Lab1")).unwrap();
    let bin_path = compiler::compile_submission(&extracted.work_dir).unwrap();

    let test_suite = TestSuite {
        problem_id: "CrashTest".to_string(),
        lab_number: Some("Lab1".to_string()),
        time_limit_ms: Some(2000),
        command_args: None,
        test_cases: vec![TestCase {
            id: "1".to_string(),
            name: "Null Pointer Dereference".to_string(),
            stdin: None,
            input_files: HashMap::new(),
            expected_stdout: "Should not print".to_string(),
            expected_regex: None,
            weight: 10.0,
        }],
    };

    let (results, _, _, passed, _) =
        runner::run_test_suite(&bin_path, &extracted.work_dir, &test_suite);

    assert_eq!(passed, 0);
    // On Windows, NTSTATUS 0xC0000005 is caught as Segmentation Fault / Access Violation
    // On Unix, SIGSEGV 11 is caught as Segmentation Fault
    assert!(
        results[0].verdict.contains("Segmentation Fault")
            || results[0].verdict.contains("Access Violation")
            || results[0].verdict.contains("Runtime Error"),
        "Verdict was: {}",
        results[0].verdict
    );

    platform::robust_cleanup(extracted.temp_dir).unwrap();
}

#[test]
fn test_reporting_json_and_csv() {
    let temp_root = tempdir().expect("create temp dir");
    let output_dir = temp_root.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let report = c_autograder::models::IndividualReport {
        student_name: "TestStudent".to_string(),
        lab_number: "Lab1".to_string(),
        submission_time: "14-11-2026 18:00".to_string(),
        zip_filename: "test.zip".to_string(),
        section: Some("Section_A".to_string()),
        problem_id: "Prob1".to_string(),
        compiled: true,
        compilation_error: None,
        total_score: 100.0,
        max_score: 100.0,
        percentage: 100.0,
        tests_passed: 5,
        total_tests: 5,
        results: vec![c_autograder::models::TestCaseResult {
            test_case_id: "1".to_string(),
            test_case_name: "Sample TC".to_string(),
            verdict: "Accepted".to_string(),
            passed: true,
            score: 20.0,
            max_score: 20.0,
            execution_time_ms: 10,
            actual_stdout: "word : better count : 2\n".to_string(),
            expected_stdout: "word : better count : 2\n".to_string(),
            stderr: String::new(),
            error_message: None,
        }],
    };

    let report_path = reporter::write_student_report(&output_dir, Some("Section_A"), &report, "test_sub").unwrap();
    assert!(report_path.exists());
    assert!(report_path.to_string_lossy().ends_with(".txt"));

    let txt_content = std::fs::read_to_string(&report_path).unwrap();
    assert!(txt_content.contains("INDIVIDUAL STUDENT EVALUATION REPORT"));
    assert!(txt_content.contains("ALL TEST CASE TERMINAL OUTPUTS"));
    assert!(txt_content.contains("--- ACTUAL TERMINAL OUTPUT ---"));

    // Test section-specific CSV generation
    let sec_row = c_autograder::models::SectionSummaryRow {
        student_name: "test_sub".to_string(),
        total_test_cases: 4,
        passed_test_cases: 3,
        percentage_marks: 75,
        scaled_down: 2,
        fixed_marks: 3,
        aloted_marks: 5,
    };
    let sec_csv_path = reporter::write_section_csv(&output_dir, "Section_A", &[sec_row]).unwrap();
    assert!(sec_csv_path.exists());

    let sec_csv_content = std::fs::read_to_string(sec_csv_path).unwrap();
    assert!(sec_csv_content.contains("student_name,total_test_cases,passed_test_cases,percentage_marks,scaled_down,fixed_marks,aloted_marks"));
    assert!(sec_csv_content.contains("test_sub,4,3,75,2,3,5"));

    let row = c_autograder::models::MasterSummaryRow {
        student_name: "TestStudent".to_string(),
        lab_number: "Lab1".to_string(),
        submission_time: "14-11-2026 18:00".to_string(),
        zip_filename: "test.zip".to_string(),
        section: Some("Section_A".to_string()),
        status: "COMPILED".to_string(),
        score: 100.0,
        max_score: 100.0,
        percentage: 100.0,
        passed_tests: 4,
        total_tests: 4,
        execution_time_total_ms: 120,
        notes: "All Tests Passed".to_string(),
        scaled_down: 2,
        fixed_marks: 3,
        aloted_marks: 5,
    };

    let csv_path = reporter::write_master_csv(&output_dir, &[row]).unwrap();
    assert!(csv_path.exists());

    let csv_content = std::fs::read_to_string(csv_path).unwrap();
    assert!(csv_content.contains("student_name,section,lab_number,submission_time,zip_filename,status,passed_tests,total_tests,execution_time_total_ms,notes,scaled_down,fixed_marks,aloted_marks"));
    assert!(csv_content.contains("TestStudent,Section_A,Lab1,14-11-2026 18:00,test.zip,COMPILED,4,4,120,All Tests Passed,2,3,5"));
}

#[test]
fn test_regex_output_matching() {
    let expected = "word : better : 3 4\nword : betty : 1 3\nword : bitter : 2 4\n";

    // 1. Exact match
    let (v1, p1, s1, n1) = runner::evaluate_with_regex(expected, expected, None, 25.0);
    assert_eq!(v1, c_autograder::models::Verdict::Accepted);
    assert!(p1);
    assert_eq!(s1, 25.0);
    assert!(n1.is_none());

    // 2. Different formatting (spacing, colons without spaces, tabs) handled by regex
    let actual_diff_format = "word: better: 3 4\nword:betty: 1\t3\nword   :   bitter   :   2 4\n";
    let (v2, p2, s2, n2) = runner::evaluate_with_regex(actual_diff_format, expected, None, 25.0);
    assert_eq!(v2, c_autograder::models::Verdict::Accepted);
    assert!(p2);
    assert_eq!(s2, 25.0);
    assert!(n2.is_some());

    // 3. Pointless print statements (prompts, debug, banners) filtered by regex
    let actual_pointless_prints = "Enter filename: input1.txt\nOpening file...\nTrie initialized successfully\nword : better : 3 4\nword : betty : 1 3\nword : bitter : 2 4\nTotal words: 3\nDone!\n";
    let (v3, p3, s3, n3) = runner::evaluate_with_regex(actual_pointless_prints, expected, None, 25.0);
    assert_eq!(v3, c_autograder::models::Verdict::Accepted);
    assert!(p3);
    assert_eq!(s3, 25.0);
    assert!(n3.is_some());

    // 4. Custom regex mapping: allowing mapping anything with anything!
    let custom_regex = r"(?s)(?i).*?better\D*3\D*4.*?betty\D*1\D*3.*";
    let weird_output = "We found better at line 3 and 4! Then found betty at 1 and 3.";
    let (v4, p4, s4, n4) = runner::evaluate_with_regex(weird_output, expected, Some(custom_regex), 25.0);
    assert_eq!(v4, c_autograder::models::Verdict::Accepted);
    assert!(p4);
    assert_eq!(s4, 25.0);
    assert!(n4.is_some());

    // 5. Genuine Wrong Answer (wrong line numbers) MUST fail
    let actual_wrong = "word : better : 3\nword : betty : 1 3\nword : bitter : 2 4\n";
    let (v5, p5, s5, _) = runner::evaluate_with_regex(actual_wrong, expected, None, 25.0);
    assert_eq!(v5, c_autograder::models::Verdict::WrongAnswer);
    assert!(!p5);
    assert_eq!(s5, 0.0);

    // 6. Extraneous wrong answer (fake word) MUST fail
    let actual_extra_word = "word : better : 3 4\nword : betty : 1 3\nword : bitter : 2 4\nword : fake : 9\n";
    let (v6, p6, s6, _) = runner::evaluate_with_regex(actual_extra_word, expected, None, 25.0);
    assert_eq!(v6, c_autograder::models::Verdict::WrongAnswer);
    assert!(!p6);
    assert_eq!(s6, 0.0);
}

#[test]
fn test_word_frequency_regex_variations() {
    let expected = "word : better count : 2\nword : betty count : 2\nword : bitter count : 2\n";

    // Format 1: No space before colon ("word: better count: 2")
    let f1 = "word: better count: 2\nword: betty count: 2\nword: bitter count: 2\n";
    let (v1, p1, s1, _) = runner::evaluate_with_regex(f1, expected, None, 25.0);
    assert_eq!(v1, c_autograder::models::Verdict::Accepted);
    assert!(p1);
    assert_eq!(s1, 25.0);

    // Format 2: Colon instead of count ("word : better : 2")
    let f2 = "word : better : 2\nword : betty : 2\nword : bitter : 2\n";
    let (v2, p2, s2, _) = runner::evaluate_with_regex(f2, expected, None, 25.0);
    assert_eq!(v2, c_autograder::models::Verdict::Accepted);
    assert!(p2);
    assert_eq!(s2, 25.0);

    // Format 3: Word omitted ("better count : 2")
    let f3 = "better count : 2\nbetty count : 2\nbitter count : 2\n";
    let (v3, p3, s3, _) = runner::evaluate_with_regex(f3, expected, None, 25.0);
    assert_eq!(v3, c_autograder::models::Verdict::Accepted);
    assert!(p3);
    assert_eq!(s3, 25.0);

    // Format 4: Pure pairs ("better : 2")
    let f4 = "better : 2\nbetty : 2\nbitter : 2\n";
    let (v4, p4, s4, _) = runner::evaluate_with_regex(f4, expected, None, 25.0);
    assert_eq!(v4, c_autograder::models::Verdict::Accepted);
    assert!(p4);
    assert_eq!(s4, 25.0);

    // Format 5: Extra blank lines & casing
    let f5 = "\n\nWord : Better Count : 2\n\nWord : Betty Count : 2\n\nWord : Bitter Count : 2\n\n";
    let (v5, p5, s5, _) = runner::evaluate_with_regex(f5, expected, None, 25.0);
    assert_eq!(v5, c_autograder::models::Verdict::Accepted);
    assert!(p5);
    assert_eq!(s5, 25.0);

    // Format 6: Pointless debug prints before and after
    let f6 = "Enter filename: input1.txt\nFile opened\nword : better count : 2\nword : betty count : 2\nword : bitter count : 2\nTotal words: 3\nDone\n";
    let (v6, p6, s6, _) = runner::evaluate_with_regex(f6, expected, None, 25.0);
    assert_eq!(v6, c_autograder::models::Verdict::Accepted);
    assert!(p6);
    assert_eq!(s6, 25.0);

    // Format 7: Wrong count must FAIL
    let f7 = "word : better count : 3\nword : betty count : 2\nword : bitter count : 2\n";
    let (v7, p7, s7, _) = runner::evaluate_with_regex(f7, expected, None, 25.0);
    assert_eq!(v7, c_autograder::models::Verdict::WrongAnswer);
    assert!(!p7);
    assert_eq!(s7, 0.0);
}
