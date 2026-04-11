use clap::Parser as ClapParser;
use tracing_subscriber::EnvFilter;

use lumen::cli::Args;
use lumen::pipeline;

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    init_tracing(&args);
    install_sigpipe_handler();

    pipeline::run(args)
}

// ── Tracing initialisation ────────────────────────────────────────────────────

fn init_tracing(args: &Args) {
    let level: &str = if args.quiet { "error" } else if args.verbose { "debug" } else { "warn" };

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber
        ::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact()
        .init();
}

// ── SIGPIPE handler ───────────────────────────────────────────────────────────

/// On Unix, Rust sets SIGPIPE to SIG_IGN by default. Restoring SIG_DFL lets
/// the process terminate silently when a pipe consumer closes early
/// (e.g. `lumen … | head -n 100`).
fn install_sigpipe_handler() {
    #[cfg(unix)]
    // SAFETY: signal() is async-signal-safe; called before any threads are
    // spawned, and only modifies the SIGPIPE disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
