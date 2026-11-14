use clap::Parser;
use std::path::PathBuf;

/// High-Performance Cross-Platform C Autograder for Windows and Linux.
#[derive(Parser, Debug, Clone)]
#[command(name = "c-autograder")]
#[command(author = "DeepMind Systems Software Engineering")]
#[command(version = "0.1.0")]
#[command(about = "Batch-evaluates undergraduate C programming submissions against a JSON test suite natively on Windows and Linux.")]
pub struct CliArgs {
    /// Directory containing student .zip submissions (accessed in strict read-only mode).
    #[arg(short = 'z', long = "zips-dir")]
    pub zips_dir: PathBuf,

    /// Path to the JSON test suite file (e.g. testcases.json).
    #[arg(short = 't', long = "testcases")]
    pub testcases: PathBuf,

    /// Output directory for student JSON reports and master CSV (default: output).
    #[arg(short = 'o', long = "output-dir", default_value = "output")]
    pub output_dir: PathBuf,

    /// Optional Lab Number override (e.g. 'Lab1'). If omitted, inferred from testcases.json or zip names.
    #[arg(short = 'l', long = "lab")]
    pub lab: Option<String>,

    /// Number of parallel worker threads (defaults to system logical CPU count).
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<usize>,
}
