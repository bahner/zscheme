//! IPC protocol between the zscheme frontend and the backend daemon.
//!
//! Wire format: each frame is a big-endian `u32` length prefix followed by a
//! CBOR-encoded `Request` or `Response`. Transport is a per-user Unix domain
//! socket — the daemon owns the single iroh endpoint for the identity, and
//! frontends (REPL / scripts) submit Scheme source for evaluation.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum accepted frame size (16 MiB) — guards against corrupt prefixes.
const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

// ── Messages ───────────────────────────────────────────────────────────────

/// Frontend → daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Version handshake; must be the first request on a connection.
    Hello { version: String },
    /// Evaluate Scheme source. `isolated` requests a per-connection
    /// environment instead of the shared session environment.
    Eval {
        id: u64,
        source: String,
        isolated: bool,
    },
    /// Liveness check.
    Ping,
    /// Ask the daemon to shut down gracefully.
    Stop,
    /// Re-initialise the shared session environment (all defines dropped).
    Reset,
    /// Serialise the shared session environment to Scheme source.
    DumpEnv,
}

/// Daemon → frontend.
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    /// Handshake reply.
    HelloAck { version: String, did: String },
    /// `(display …)` output produced while evaluating request `id`.
    Display { id: u64, text: String },
    /// Final outcome of request `id`. `Ok(None)` means the result was nil
    /// (nothing to print); `Err` carries a pre-formatted error message.
    EvalResult {
        id: u64,
        outcome: Result<Option<String>, String>,
    },
    /// Reply to `Ping`.
    Pong,
    /// Acknowledgement of `Stop`; the daemon exits after sending this.
    Stopping,
    /// Acknowledgement of `Reset`.
    ResetDone,
    /// Reply to `DumpEnv`: the session environment as Scheme source.
    EnvDump { source: String },
}

// ── Socket path ────────────────────────────────────────────────────────────

/// Resolve the per-user daemon socket path.
///
/// Prefers `$XDG_RUNTIME_DIR/zscheme.sock`; falls back to
/// `<data dir>/ma/zscheme.sock` when no runtime dir is available.
///
/// # Errors
///
/// Returns an error if the home/data directory cannot be resolved or the
/// fallback socket directory cannot be created.
pub fn socket_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| anyhow!("cannot resolve home dir"))?;
    if let Some(runtime) = base.runtime_dir() {
        return Ok(runtime.join("zscheme.sock"));
    }
    let dir = base.data_dir().join("ma");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir.join("zscheme.sock"))
}

/// Path to the daemon log file (stdout/stderr of auto-spawned daemons).
///
/// # Errors
///
/// Returns an error if the home/data directory cannot be resolved or the log
/// directory cannot be created.
pub fn daemon_log_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| anyhow!("cannot resolve home dir"))?;
    let dir = base.data_dir().join("ma");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir.join("zscheme-daemon.log"))
}

// ── Framing ────────────────────────────────────────────────────────────────

/// Write one CBOR frame (length-prefixed) to `writer`.
///
/// # Errors
///
/// Returns an error if CBOR encoding fails, the frame is too large, or writing
/// to the stream fails.
pub async fn write_frame<W, T>(writer: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut buf = Vec::new();
    ciborium::into_writer(msg, &mut buf).context("cbor encode")?;
    let len = u32::try_from(buf.len()).context("frame too large")?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one CBOR frame from `reader`. Returns `Ok(None)` on clean EOF.
///
/// # Errors
///
/// Returns an error if reading fails, the frame exceeds the maximum size, or
/// CBOR decoding fails.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncReadExt + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_LEN {
        return Err(anyhow!("IPC frame too large: {len} bytes"));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    let msg = ciborium::from_reader(buf.as_slice()).context("cbor decode")?;
    Ok(Some(msg))
}
