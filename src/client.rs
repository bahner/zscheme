//! Frontend IPC client for the zscheme backend daemon.
//!
//! The default `zscheme` invocation (REPL or script) does not load the
//! secret bundle or create an iroh endpoint. Instead it connects to the
//! per-user daemon over a Unix socket — auto-spawning the daemon if it is
//! not already running — and submits Scheme source for evaluation.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::UnixStream;

use crate::ipc::{daemon_log_path, read_frame, socket_path, write_frame, Request, Response};
use crate::repl::{self, ReplEval};

/// How many times to retry connecting after auto-spawning the daemon.
const SPAWN_RETRIES: u32 = 40;
/// Delay between connection retries.
const SPAWN_RETRY_DELAY: Duration = Duration::from_millis(250);

// ── Client ─────────────────────────────────────────────────────────────────

pub struct DaemonClient {
    stream: UnixStream,
    next_id: u64,
    isolated: bool,
    /// Whether the last `Eval` produced display output that did not end
    /// with a newline (used by the REPL to keep the prompt on its own line).
    dangling_output: bool,
}

impl DaemonClient {
    /// Connect to the daemon, auto-spawning it if needed, and perform the
    /// version handshake.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket path cannot be resolved, the daemon
    /// cannot be started or reached, or the version handshake fails.
    pub async fn connect_or_spawn(isolated: bool) -> Result<Self> {
        let path = socket_path()?;
        let stream = if let Ok(stream) = UnixStream::connect(&path).await {
            stream
        } else {
            spawn_daemon()?;
            let mut stream = None;
            for _ in 0..SPAWN_RETRIES {
                tokio::time::sleep(SPAWN_RETRY_DELAY).await;
                if let Ok(s) = UnixStream::connect(&path).await {
                    stream = Some(s);
                    break;
                }
            }
            stream.ok_or_else(|| {
                anyhow!(
                    "zscheme daemon failed to start — check {}",
                    daemon_log_path().map_or_else(
                        |_| "the daemon log".to_string(),
                        |p| p.display().to_string()
                    )
                )
            })?
        };

        let mut client = Self {
            stream,
            next_id: 1,
            isolated,
            dangling_output: false,
        };
        client.hello().await?;
        Ok(client)
    }

    async fn hello(&mut self) -> Result<()> {
        let version = env!("CARGO_PKG_VERSION").to_string();
        write_frame(&mut self.stream, &Request::Hello { version }).await?;
        let resp: Response = read_frame(&mut self.stream)
            .await?
            .ok_or_else(|| anyhow!("daemon closed connection during handshake"))?;
        match resp {
            Response::HelloAck { version, .. } => {
                if version != env!("CARGO_PKG_VERSION") {
                    bail!(
                        "daemon version {version} does not match client {} — \
                         run `zscheme --stop` and try again",
                        env!("CARGO_PKG_VERSION")
                    );
                }
                Ok(())
            }
            other => Err(anyhow!("unexpected handshake reply: {other:?}")),
        }
    }

    /// Evaluate `source` in the daemon. Streams `(display …)` output to
    /// stdout as it arrives; returns the final outcome.
    ///
    /// Outer `Err` = IPC failure; inner `Err` = Scheme evaluation error
    /// (pre-formatted message).
    ///
    /// # Errors
    ///
    /// Returns an error if writing the request fails, the daemon closes the
    /// connection unexpectedly, or a response frame cannot be decoded.
    pub async fn eval(&mut self, source: &str) -> Result<Result<Option<String>, String>> {
        let id = self.next_id;
        self.next_id += 1;
        self.dangling_output = false;
        write_frame(
            &mut self.stream,
            &Request::Eval {
                id,
                source: source.to_string(),
                isolated: self.isolated,
            },
        )
        .await?;

        loop {
            let resp: Response = read_frame(&mut self.stream)
                .await?
                .ok_or_else(|| anyhow!("daemon closed connection"))?;
            match resp {
                Response::Display { id: rid, text } if rid == id => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                    if !text.is_empty() {
                        self.dangling_output = !text.ends_with('\n');
                    }
                }
                Response::EvalResult { id: rid, outcome } if rid == id => return Ok(outcome),
                _ => {}
            }
        }
    }
}

// ── Entry points ───────────────────────────────────────────────────────────

/// Default frontend entry point: run a script or the REPL against the daemon.
///
/// # Errors
///
/// Returns an error if the daemon connection fails, the script cannot be read,
/// IPC fails, or Scheme evaluation reports an error.
pub async fn run(script: Option<PathBuf>, isolated: bool) -> Result<()> {
    let client = DaemonClient::connect_or_spawn(isolated).await?;

    if let Some(ref path) = script {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read script: {}", path.display()))?;
        let source = crate::executor::strip_shebang(&raw);
        let mut client = client;
        match client.eval(source).await? {
            Ok(_) => Ok(()),
            Err(e) => {
                eprintln!("{e}");
                Err(anyhow!("{e}"))
            }
        }
    } else {
        repl::run_repl(ClientEval(client)).await
    }
}

/// Ask a running daemon to shut down.
///
/// # Errors
///
/// Returns an error if the daemon socket cannot be resolved or IPC with the
/// running daemon fails.
pub async fn stop() -> Result<()> {
    let Some(mut stream) = connect_existing().await? else {
        eprintln!("no zscheme daemon running");
        return Ok(());
    };
    write_frame(&mut stream, &Request::Stop).await?;
    while let Some(resp) = read_frame::<_, Response>(&mut stream).await? {
        if matches!(resp, Response::Stopping) {
            break;
        }
    }
    eprintln!("zscheme daemon stopped");
    Ok(())
}

/// Reset the daemon's shared session environment.
///
/// # Errors
///
/// Returns an error if the daemon socket cannot be resolved, IPC fails, or the
/// daemon closes the connection before acknowledging the reset.
pub async fn reset() -> Result<()> {
    let Some(mut stream) = connect_existing().await? else {
        eprintln!("no zscheme daemon running — nothing to reset");
        return Ok(());
    };
    write_frame(&mut stream, &Request::Reset).await?;
    while let Some(resp) = read_frame::<_, Response>(&mut stream).await? {
        if matches!(resp, Response::ResetDone) {
            eprintln!("session environment reset");
            return Ok(());
        }
    }
    Err(anyhow!("daemon closed connection before confirming reset"))
}

/// Save the daemon's session environment as Scheme source to `file`,
/// or to stdout when no file is given.
///
/// # Errors
///
/// Returns an error if no daemon is running, IPC fails, the daemon closes the
/// connection before sending the dump, or the output file cannot be written.
pub async fn save(file: Option<PathBuf>) -> Result<()> {
    let Some(mut stream) = connect_existing().await? else {
        bail!("no zscheme daemon running — nothing to save");
    };
    write_frame(&mut stream, &Request::DumpEnv).await?;
    while let Some(resp) = read_frame::<_, Response>(&mut stream).await? {
        if let Response::EnvDump { source } = resp {
            match file {
                Some(path) => {
                    std::fs::write(&path, source)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                    eprintln!("session saved to {}", path.display());
                }
                None => print!("{source}"),
            }
            return Ok(());
        }
    }
    Err(anyhow!("daemon closed connection before sending the dump"))
}

/// Connect to a running daemon, or `None` if the socket is dead.
async fn connect_existing() -> Result<Option<UnixStream>> {
    let path = socket_path()?;
    Ok(UnixStream::connect(&path).await.ok())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Spawn a detached `zscheme --daemon` process, logging to the daemon log
/// file. The child inherits the environment (including the secret bundle
/// passphrase).
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe().context("cannot resolve zscheme executable path")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path()?)?;
    let log_err = log.try_clone()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // detach from the frontend's TTY signals
    }
    cmd.spawn().context("cannot spawn zscheme daemon")?;
    Ok(())
}

/// `ReplEval` adapter for the daemon client.
struct ClientEval(DaemonClient);

impl ReplEval for ClientEval {
    async fn eval(&mut self, source: &str) -> Result<Option<String>, String> {
        let outcome = match self.0.eval(source).await {
            Ok(outcome) => outcome,
            Err(e) => Err(format!("ipc error: {e}")),
        };
        // (display …) without a trailing newline would otherwise be
        // overwritten by the next readline prompt redraw.
        if self.0.dangling_output {
            println!();
            self.0.dangling_output = false;
        }
        outcome
    }
}
