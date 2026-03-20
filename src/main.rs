// `pyfreeze` CLI  —  the drop-in replacement for the `python` command.
//
// Usage:
//   pyfreeze run  manage.py runserver           # Django
//   pyfreeze run  -m flask run                  # Flask
//   pyfreeze info myapp.pyfreeze                # inspect a snapshot
//   pyfreeze invalidate myapp.pyfreeze          # force-delete a snapshot
//   pyfreeze benchmark manage.py runserver      # compare cold vs warm startup

mod error;
mod snapshot;
mod capture;
mod loader;

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

// ─── CLI definition ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "pyfreeze",
    version = env!("CARGO_PKG_VERSION"),
    about   = "Dormant Process Resumption for Python — CPython import-phase snapshots",
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Python script or module, using the snapshot if available.
    Run {
        /// Python script or `-m module` to run.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        /// Override the snapshot file path (default: auto-derived from script + hash).
        #[arg(long, short = 's')]
        snapshot: Option<PathBuf>,

        /// Framework hint: "django" | "flask" | "generic".
        #[arg(long, short = 'f', default_value = "generic")]
        framework: String,

        /// Force a fresh capture even if a valid snapshot exists.
        #[arg(long)]
        rebuild: bool,
    },

    /// Print human-readable information about a snapshot file.
    Info {
        snapshot: PathBuf,
    },

    /// Delete a snapshot (force rebuild on next run).
    Invalidate {
        snapshot: PathBuf,
    },

    /// Run the script twice (cold then warm) and print the timing difference.
    Benchmark {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        #[arg(long, short = 'f', default_value = "generic")]
        framework: String,
    },
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    // Initialise tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PYFREEZE_LOG")
                .unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    let exit_code = match cli.command {
        Commands::Run { args, snapshot, framework, rebuild } => {
            cmd_run(args, snapshot, framework, rebuild)
        }
        Commands::Info { snapshot } => cmd_info(&snapshot),
        Commands::Invalidate { snapshot } => cmd_invalidate(&snapshot),
        Commands::Benchmark { args, framework } => cmd_benchmark(args, framework),
    };

    process::exit(exit_code);
}

// ─── `run` subcommand ─────────────────────────────────────────────────────────

fn cmd_run(
    args:      Vec<String>,
    snapshot:  Option<PathBuf>,
    framework: String,
    rebuild:   bool,
) -> i32 {
    if args.is_empty() {
        eprintln!("error: no Python script or module specified");
        return 2;
    }

    // Derive snapshot path from the script name if not explicitly given.
    let snap_path = snapshot.unwrap_or_else(|| derive_snapshot_path(&args, &framework));

    // Delegate to Python; we inject pyfreeze into the process via PYTHONPATH
    // pointing to our Python wrapper package.
    let python_exe = find_python();
    let pyfreeze_hook = build_hook_code(&snap_path, &framework, rebuild);

    let status = process::Command::new(&python_exe)
        .arg("-c")
        .arg(pyfreeze_hook)
        .args(&args)
        // Make our compiled extension importable.
        .env("PYFREEZE_SNAPSHOT", snap_path.to_string_lossy().as_ref())
        .env("PYFREEZE_FRAMEWORK", &framework)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("error: could not launch Python ({python_exe}): {e}");
            process::exit(1);
        });

    status.code().unwrap_or(1)
}

/// Python bootstrap injected before the user's script.
/// This is intentionally tiny — the heavy lifting is in the Python `pyfreeze` package.
fn build_hook_code(snap_path: &PathBuf, framework: &str, rebuild: bool) -> String {
    format!(
        r#"
import sys, os
# Insert pyfreeze Python package (installed alongside the binary).
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'python'))
import pyfreeze
pyfreeze.bootstrap(
    snapshot_path={snap_path:?},
    framework={framework:?},
    rebuild={rebuild},
    argv=sys.argv[1:],
)
"#,
        snap_path = snap_path.to_string_lossy(),
        framework = framework,
        rebuild   = if rebuild { "True" } else { "False" },
    )
}

// ─── `info` subcommand ────────────────────────────────────────────────────────

fn cmd_info(path: &PathBuf) -> i32 {
    match snapshot::SnapshotReader::open(path) {
        Err(e) => {
            eprintln!("error reading snapshot: {e}");
            1
        }
        Ok(reader) => {
            let m = &reader.header.metadata;
            println!("PyFreeze Snapshot");
            println!("  Path          : {}", path.display());
            println!("  Python        : {}", m.python_impl_version);
            println!("  Target        : {}", m.target_triple);
            println!("  Framework     : {}", m.framework);
            println!("  Captured at   : {}", m.captured_at);
            println!("  Import phase  : {}ms", m.import_phase_ms);
            println!("  Source hash   : {}", m.source_hash);
            println!("  Modules total : {}", reader.module_table.entries.len());

            let pickled = reader.module_table.entries.iter()
                .filter(|e| e.capture_strategy == snapshot::format::CaptureStrategy::PickledDict)
                .count();
            let reimport = reader.module_table.entries.iter()
                .filter(|e| e.capture_strategy == snapshot::format::CaptureStrategy::ReImport)
                .count();

            println!("    ├─ pickled   : {}", pickled);
            println!("    └─ re-import : {}", reimport);
            println!("  FDs captured  : {}", reader.fd_table.entries.len());
            0
        }
    }
}

// ─── `invalidate` subcommand ─────────────────────────────────────────────────

fn cmd_invalidate(path: &PathBuf) -> i32 {
    match std::fs::remove_file(path) {
        Ok(()) => {
            println!("Snapshot deleted: {}", path.display());
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("No snapshot found at {}", path.display());
            1
        }
        Err(e) => {
            eprintln!("Error deleting snapshot: {e}");
            1
        }
    }
}

// ─── `benchmark` subcommand ──────────────────────────────────────────────────

fn cmd_benchmark(args: Vec<String>, framework: String) -> i32 {
    use std::time::Instant;

    let snap_path = derive_snapshot_path(&args, &framework);
    let python_exe = find_python();

    // Cold run — delete snapshot first.
    let _ = std::fs::remove_file(&snap_path);
    let t0 = Instant::now();
    let _ = process::Command::new(&python_exe)
        .env("PYFREEZE_SNAPSHOT", snap_path.to_string_lossy().as_ref())
        .env("PYFREEZE_FRAMEWORK", &framework)
        .args(&args)
        .status();
    let cold_ms = t0.elapsed().as_millis();

    // Warm run — snapshot was written by cold run.
    let t1 = Instant::now();
    let _ = process::Command::new(&python_exe)
        .env("PYFREEZE_SNAPSHOT", snap_path.to_string_lossy().as_ref())
        .env("PYFREEZE_FRAMEWORK", &framework)
        .args(&args)
        .status();
    let warm_ms = t1.elapsed().as_millis();

    println!("┌─────────────────────────────────────┐");
    println!("│        PyFreeze Benchmark            │");
    println!("├─────────────────────────────────────┤");
    println!("│  Cold start : {:>6} ms             │", cold_ms);
    println!("│  Warm start : {:>6} ms             │", warm_ms);
    println!("│  Speedup    : {:>6.1}x              │", cold_ms as f64 / warm_ms as f64);
    println!("└─────────────────────────────────────┘");

    0
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn find_python() -> String {
    // Prefer the interpreter that's on PATH as "python3".
    for candidate in &["python3", "python"] {
        if which(*candidate) {
            return candidate.to_string();
        }
    }
    "python3".to_string()
}

fn which(name: &str) -> bool {
    process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn derive_snapshot_path(args: &[String], framework: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = format!("{}-{}", args.join(" "), framework);
    let hash = hex::encode(&Sha256::digest(key.as_bytes())[..8]);
    snapshot::default_cache_dir().join(format!("{}.pyfreeze", hash))
}
