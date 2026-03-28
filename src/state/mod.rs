use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::scorer::cms::CountMinSketch;

// ── State directory resolution (§14, §18.4) ───────────────────────────────────

/// Returns the platform-appropriate base directory for lumen state files:
///
/// | Platform | Path |
/// |----------|------|
/// | Linux    | `$XDG_STATE_HOME/lumen` or `~/.local/state/lumen` |
/// | macOS    | `~/Library/Application Support/lumen` |
/// | Windows  | `%LOCALAPPDATA%\lumen` |
pub fn base_state_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lumen")
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lumen")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // XDG_STATE_HOME takes precedence; fall back to ~/.local/state
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
            })
            .join("lumen")
    }
}

/// Returns the project-specific state directory: `<base>/projects/<slug>`.
pub fn project_state_dir(slug: &str) -> PathBuf {
    base_state_dir().join("projects").join(slug)
}

// ── Run metadata ──────────────────────────────────────────────────────────────

/// Serialisable metadata written to `meta.json` after each successful run (§14).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    /// Monotonically increasing run counter.
    pub run_index: u64,
    /// Unix timestamp (seconds) of this run.
    pub run_ts: i64,
    /// Source file name (or `"<stdin>"`).
    pub source_name: String,
    /// Total bytes processed.
    pub total_bytes: u64,
    /// Total templates produced.
    pub template_count: u64,
    /// Whether the Zstd dictionary was trained or retrained on this run.
    pub dict_trained: bool,
}

// ── State store ───────────────────────────────────────────────────────────────

/// Manages all persistent lumen state for a project (§14):
///
/// ```text
/// <state_dir>/
/// ├── meta.json        RunMeta for the last run
/// ├── cms.bin          CRC32-headered Count-Min Sketch dump
/// ├── dict.zst         Trained Zstd dictionary (absent on first run)
/// └── templates.bin    (reserved for future template persistence)
/// ```
pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    /// Open (or create) the state directory for `project_slug`.
    pub fn open(project_slug: &str) -> Result<Self> {
        let dir = project_state_dir(project_slug);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating state directory: {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// Delete all state files in the project directory (triggered by `--reset-state`).
    pub fn reset(&self) -> Result<()> {
        for name in &["meta.json", "cms.bin", "dict.zst", "templates.bin"] {
            let path = self.dir.join(name);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        tracing::info!("State reset: removed all files in {}", self.dir.display());
        Ok(())
    }

    // ── meta.json ─────────────────────────────────────────────────────────────

    pub fn load_meta(&self) -> Result<Option<RunMeta>> {
        let path = self.dir.join("meta.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let meta = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(meta))
    }

    pub fn save_meta(&self, meta: &RunMeta) -> Result<()> {
        let path = self.dir.join("meta.json");
        let bytes = serde_json::to_vec_pretty(meta).context("serialising meta.json")?;
        atomic_write(&path, &bytes)
    }

    // ── cms.bin ───────────────────────────────────────────────────────────────

    /// Load the CMS from `cms.bin`.
    ///
    /// Returns `None` (and logs a warning) if:
    /// - the file does not exist (first run), or
    /// - the CRC32 checksum does not match (corrupt file → rebuild from zero).
    pub fn load_cms(&self) -> Result<Option<CountMinSketch>> {
        let path = self.dir.join("cms.bin");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading {}", path.display()))?;

        if bytes.len() < 4 {
            tracing::warn!("cms.bin too short — rebuilding");
            return Ok(None);
        }

        // Verify CRC32 header (first 4 bytes).
        let stored_crc  = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let payload     = &bytes[4..];
        let computed    = crc32fast::hash(payload);

        if stored_crc != computed {
            tracing::warn!(
                path = %path.display(),
                stored_crc,
                computed,
                "cms.bin CRC32 mismatch — rebuilding sketch"
            );
            return Ok(None);
        }

        match CountMinSketch::from_bytes(payload) {
            Some(cms) => Ok(Some(cms)),
            None => {
                tracing::warn!("cms.bin has invalid magic — rebuilding sketch");
                Ok(None)
            }
        }
    }

    /// Persist `cms` to `cms.bin` with a 4-byte CRC32 prefix (§14).
    pub fn save_cms(&self, cms: &CountMinSketch) -> Result<()> {
        let payload = cms.to_bytes();
        let crc = crc32fast::hash(&payload);
        let mut buf = Vec::with_capacity(4 + payload.len());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&payload);
        atomic_write(&self.dir.join("cms.bin"), &buf)
    }

    // ── dict.zst ─────────────────────────────────────────────────────────────

    /// Load the Zstd compression dictionary, if one exists (§9.3).
    pub fn load_dict(&self) -> Result<Option<Vec<u8>>> {
        let path = self.dir.join("dict.zst");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(Some(bytes))
    }

    /// Persist a trained Zstd dictionary (§9.3).
    pub fn save_dict(&self, dict: &[u8]) -> Result<()> {
        atomic_write(&self.dir.join("dict.zst"), dict)
    }
}

// ── Atomic write helper ───────────────────────────────────────────────────────

/// Write `data` to `path` atomically: write to `<path>.tmp`, then rename.
///
/// This prevents partial writes from corrupting state files if lumen is
/// interrupted (e.g. by SIGPIPE).
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(data)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.flush().with_context(|| format!("flushing {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))
}
