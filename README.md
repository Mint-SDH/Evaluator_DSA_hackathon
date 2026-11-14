# c-autograder: High-Performance Cross-Platform C Autograder

`c-autograder` is a high-throughput, memory-safe, cross-platform C programming autograder written in Rust. It evaluates student `.zip` submissions in parallel against a JSON test suite natively on both **Windows 10/11** and **Linux (Ubuntu/Debian/Fedora/etc.)** without modifying a single line of source code.

---

## 1. Architectural Highlights: Cross-Platform OS Abstraction

The autograder handles all 6 critical OS differences via its dedicated abstraction layer (`src/platform.rs`):

| # | OS Difference | Windows vs. Linux Behavior | Architecture Solution in `c-autograder` |
|---|---|---|---|
| **1** | **NTFS File Locking on Cleanup** | On Windows, killed `.exe` processes hold kernel file locks for 10–50ms. Immediate folder deletion throws `OS Error 5 / 32 (Access Denied)`. | Drops all pipe handles, calls `child.kill()` followed immediately by `child.wait()`, and invokes `platform::robust_cleanup()` with a 5-step backoff retry loop (30ms sleep). Extracted folders are 100% wiped. |
| **2** | **Bypassing Broken Student Makefiles** | Student makefiles often contain OS-specific shell commands (`rm -f *.o` fails on Windows `cmd.exe`; `del` fails on Linux). | **Stage 1 (Primary):** Shell-free compilation via `std::process::Command::new("gcc")` directly, discovering all `.c` and `.h` directories via `walkdir`.<br>**Stage 2 (Fallback):** Attempts `make` (or `mingw32-make` on Windows). |
| **3** | **Executable Extensions & Paths** | Windows requires `.exe` and `\`; Linux requires `./` and `/`. | Uses `std::env::consts::EXE_EXTENSION` and `std::path::PathBuf` so binary names (`student_bin.exe` vs `student_bin`) adapt at compile time. |
| **4** | **Segfault & Crash Detection** | Linux reports segfaults via POSIX signals (`SIGSEGV` = 11, `SIGFPE` = 8). Windows reports hexadecimal NTSTATUS exit codes (`0xC0000005` Access Violation, `0xC00000FD` Stack Overflow, `0xC0000094` Divide by Zero). | Platform abstraction matches NTSTATUS signed exit codes on Windows and POSIX signals on Unix. Also sets Windows `SetErrorMode(SEM_NOGPFAULTERRORBOX)` so Windows never hangs waiting for crash dialogs. |
| **5** | **CRLF (`\r\n`) vs LF (`\n`)** | Windows C binaries emit `\r\n` on stdout; Linux emits `\n`. | Runner normalizes in memory: splits lines, strips trailing whitespace per line, rejoins with `\n`, and strips EOF newlines before diffing. Same `testcases.json` runs identically on both OSes. |
| **6** | **Windows Defender & Antivirus Lag** | Scanning 2,600 newly generated `.exe` files in system `%TEMP%` introduces up to 300ms lag per test. | Extracts into `<output-dir>/.temp_sandbox/` using `tempdir_in`. This single folder can be added to Windows Defender exclusions for a 5x speed boost, and is auto-deleted upon completion. |

---

## 2. Project Directory Structure

```text
c-autograder/
├── Cargo.toml                       # Dependencies (clap, rayon, zip, walkdir, tempfile, wait-timeout, etc.)
├── src/
│   ├── main.rs                      # CLI entry point, Rayon parallel work-stealing pool, progress bar
│   ├── config.rs                    # Command line arguments (--zips-dir, --testcases, --output-dir, --lab)
│   ├── models.rs                    # Data schemas for TestSuite, TestCase, StudentMetadata, Reports
│   ├── platform.rs                  # OS Abstraction (Windows NTSTATUS vs POSIX signals, robust NTFS cleanup)
│   ├── extractor.rs                 # Read-only zip reader, metadata parser, Zip-Slip security sanitizer
│   ├── compiler.rs                  # Cross-platform C compiler (Direct GCC + Makefile fallback)
│   ├── runner.rs                    # Sandboxed runner, 4KB pipe deadlock prevention, timeout watchdog
│   ├── reporter.rs                  # Generates individual student JSON reports and master CSV
│   └── lib.rs                       # Library export root for unit and integration testing
│
├── tests/
│   ├── autograder_tests.rs          # Full integration test suite (multi-file C, timeouts, segfaults)
│   └── create_sample_workspace.rs   # Test utility for generating mock student submissions
│
└── workspace/                       # Runtime Grading Directory
    ├── .temp_sandbox/               # Ephemeral sandbox (Auto-created & cleaned up)
    ├── submissions_zip/             # INPUT: Student .zip archives (STRICT READ-ONLY, NEVER modified)
    ├── testcases.json               # INPUT: Test suite configuration and expected outputs
    └── output/                      # OUTPUT: Evaluation artifacts
        ├── master_report.csv        # Comprehensive CSV summary for all submissions
        └── student_reports/         # Detailed individual JSON breakdown per submission
            ├── PES2UG23CS001_StudentA_Lab1_Sub1_report.json
            └── ...
```

---

## 3. Host Prerequisites

### Windows (10 / 11)
1. **Rust Toolchain:** Install via [rustup.rs](https://rustup.rs).
2. **GCC & Make:** Install MinGW-w64 (via MSYS2 or WinLibs). Ensure `gcc.exe` and `mingw32-make.exe` (or `make.exe`) are in your Windows `PATH`.
   - Verify: `gcc --version`

### Linux (Ubuntu / Debian / Fedora)
1. **Rust Toolchain:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2. **GCC & Make:** `sudo apt-get install build-essential`
   - Verify: `gcc --version && make --version`

---

## 4. Build Instructions

### Build Debug Binary
```bash
cargo build
```

### Build Optimized Release Binary
```bash
cargo build --release
```
The compiled binary will be placed at:
- **Windows:** `target/release/c-autograder.exe`
- **Linux:** `target/release/c-autograder`

---

## 5. Running the Autograder

### Windows (PowerShell)
```powershell
# Run using cargo:
cargo run --release -- `
    --zips-dir workspace/submissions_zip `
    --testcases workspace/testcases.json `
    --output-dir workspace/output `
    --lab Lab1

# Or run the compiled standalone binary directly:
.\target\release\c-autograder.exe `
    --zips-dir workspace\submissions_zip `
    --testcases workspace\testcases.json `
    --output-dir workspace\output `
    --lab Lab1
```

### Windows (Command Prompt - CMD)
```cmd
target\release\c-autograder.exe ^
    --zips-dir workspace\submissions_zip ^
    --testcases workspace\testcases.json ^
    --output-dir workspace\output ^
    --lab Lab1
```

### Linux (Bash)
```bash
# Run using cargo:
cargo run --release -- \
    --zips-dir workspace/submissions_zip \
    --testcases workspace/testcases.json \
    --output-dir workspace/output \
    --lab Lab1

# Or run the compiled standalone binary directly:
./target/release/c-autograder \
    --zips-dir workspace/submissions_zip \
    --testcases workspace/testcases.json \
    --output-dir workspace/output \
    --lab Lab1
```

### CLI Command Options
- `-z, --zips-dir <PATH>`: Directory containing student `.zip` submissions (read-only mode).
- `-t, --testcases <PATH>`: Path to the `testcases.json` test suite file.
- `-o, --output-dir <PATH>`: Destination directory for reports (default: `output`).
- `-l, --lab <STRING>`: Optional lab identifier override (e.g. `Lab1`).
- `-j, --jobs <NUM>`: Number of parallel worker threads (defaults to system CPU core count).

---

## 6. Running the Automated Test Suite

To run all automated integration and unit tests:
```bash
cargo test -- --nocapture
```

The test suite automatically tests:
- Binary name resolution across platforms (`.exe` on Windows, none on Linux)
- CRLF and LF newline normalization and whitespace trimming
- Multi-file C compilation and execution (Trie and client modules)
- Process timeout watchdog and NTFS file-lock release
- Segfault / Access Violation (NTSTATUS `0xC0000005` & POSIX `SIGSEGV`) detection
- Report generation for JSON and CSV schemas
