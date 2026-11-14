use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

fn default_weight() -> f64 {
    1.0
}

fn deserialize_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IdValue {
        String(String),
        Int(i64),
        Float(f64),
    }

    match IdValue::deserialize(deserializer)? {
        IdValue::String(s) => Ok(s),
        IdValue::Int(i) => Ok(i.to_string()),
        IdValue::Float(f) => Ok(f.to_string()),
    }
}

/// The entire test suite for the problem / lab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuite {
    pub problem_id: String,
    #[serde(default)]
    pub lab_number: Option<String>,
    #[serde(default)]
    pub time_limit_ms: Option<u64>,
    #[serde(default)]
    pub command_args: Option<Vec<String>>,
    pub test_cases: Vec<TestCase>,
}

/// A single test case in the test suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    #[serde(deserialize_with = "deserialize_id")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default)]
    pub input_files: HashMap<String, String>,
    pub expected_stdout: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

/// Outcome of running a single test case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    Accepted,
    WrongAnswer,
    TimeLimitExceeded,
    RuntimeError(String),
    CompilationError(String),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Accepted => write!(f, "Accepted"),
            Verdict::WrongAnswer => write!(f, "Wrong Answer"),
            Verdict::TimeLimitExceeded => write!(f, "Time Limit Exceeded"),
            Verdict::RuntimeError(msg) => write!(f, "Runtime Error ({msg})"),
            Verdict::CompilationError(msg) => write!(f, "Compilation Error ({msg})"),
        }
    }
}

/// Detailed result of evaluating a single test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseResult {
    pub test_case_id: String,
    pub test_case_name: String,
    pub verdict: String,
    pub passed: bool,
    pub score: f64,
    pub max_score: f64,
    pub execution_time_ms: u128,
    pub actual_stdout: String,
    pub expected_stdout: String,
    pub stderr: String,
    pub error_message: Option<String>,
}

/// Metadata extracted from submission archive and filenames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentMetadata {
    pub lab_number: String,
    pub student_name: String,
    pub submission_time: String,
    pub zip_filename: String,
}

/// Comprehensive individual report written to JSON per student submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndividualReport {
    pub student_name: String,
    pub lab_number: String,
    pub submission_time: String,
    pub zip_filename: String,
    pub problem_id: String,
    pub compiled: bool,
    pub compilation_error: Option<String>,
    pub total_score: f64,
    pub max_score: f64,
    pub percentage: f64,
    pub tests_passed: usize,
    pub total_tests: usize,
    pub results: Vec<TestCaseResult>,
}

/// Summary row aggregated into the master CSV file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterSummaryRow {
    pub student_name: String,
    pub lab_number: String,
    pub submission_time: String,
    pub zip_filename: String,
    pub status: String,
    pub score: f64,
    pub max_score: f64,
    pub percentage: f64,
    pub passed_tests: usize,
    pub total_tests: usize,
    pub execution_time_total_ms: u128,
    pub notes: String,
}
