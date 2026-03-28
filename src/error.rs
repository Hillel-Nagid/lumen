use std::path::PathBuf;

/// Convenience alias — most functions in lumen return `anyhow::Result<T>`.
pub type Result<T> = anyhow::Result<T>;

/// Domain-specific errors that carry structured context beyond a plain message.
#[derive(Debug)]
pub enum LumenError {
    /// Underlying OS I/O failure.
    Io(std::io::Error),
    /// The input could not be parsed (format/encoding issue).
    Parse(String),
    /// A JSON document was malformed.
    Json(String),
    /// A state file (CMS, dict, meta) failed its CRC32 checksum — rebuild from scratch.
    CorruptState { path: PathBuf },
    /// RSS memory exceeded the configured `--memory-limit`.
    OomProtection { rss_mb: u64, limit_mb: u64 },
    /// Generic state-management failure (missing directory, permission error, etc.).
    State(String),
}

impl std::fmt::Display for LumenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Parse(msg) => write!(f, "Parse error: {msg}"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
            Self::CorruptState { path } => {
                write!(f, "Corrupt state file: {} — rebuilding", path.display())
            }
            Self::OomProtection { rss_mb, limit_mb } => {
                write!(f, "RSS {rss_mb} MB exceeded memory limit {limit_mb} MB")
            }
            Self::State(msg) => write!(f, "State error: {msg}"),
        }
    }
}

impl std::error::Error for LumenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LumenError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
