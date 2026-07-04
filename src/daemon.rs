//! zscheme backend daemon.
//!
//! Owns the single iroh endpoint for the user's identity and evaluates
//! Scheme source submitted by frontend clients over a Unix domain socket.
//! This guarantees exactly one iroh NodeId per identity regardless of how
//! many concurrent `zscheme` frontends are running.
//!
//! Evaluations are serialized FIFO across all connections (the Scheme
//! environment is single-threaded by design). Each `Eval` request streams
//! `Display` events back to its originating client, followed by a final
//! `EvalResult`.

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::spawn_local;
use tracing::{info, warn};

use crate::context::{CliCtx, Ctx};
use crate::ipc::{read_frame, socket_path, write_frame, Request, Response};
use crate::scheme::{SchemeErr, SchemeVal};

/// Run the daemon accept loop. Assumes the caller already built the full
/// `CliCtx` (endpoint, inbox, config) and started the RPC reply poll loop.
///
/// If another daemon is running it is asked to stop first (takeover).
/// `img`, when given, is a session-image file: it is evaluated into the
/// shared environment at startup (if it exists) and rewritten on shutdown.
///
/// Returns when a client sends `Stop` or the process receives
/// SIGINT/SIGTERM. The caller is responsible for closing the endpoint.
pub async fn run(ctx: Rc<CliCtx>, img: Option<PathBuf>) -> Result<()> {
    let path = socket_path()?;
    claim_socket(&path).await?;

    if let Some(ref img_path) = img {
        if img_path.exists() {
            let source = std::fs::read_to_string(img_path)
                .with_context(|| format!("cannot read image {}", img_path.display()))?;
            let scheme_ctx: Ctx = ctx.clone();
            match ma_zscheme::eval_source(&source, scheme_ctx).await {
                Ok(_) => info!(img = %img_path.display(), "session image loaded"),
                Err(e) => warn!(img = %img_path.display(), "session image failed: {e}"),
            }
        }
    }

    let listener =
        UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    info!(socket = %path.display(), did = %ctx.our_did, "zscheme daemon listening");

    let stop = Rc::new(Notify::new());
    let eval_lock = Rc::new(tokio::sync::Mutex::new(()));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("cannot install SIGTERM handler")?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let ctx = ctx.clone();
                        let stop = stop.clone();
                        let eval_lock = eval_lock.clone();
                        spawn_local(async move {
                            if let Err(e) = handle_conn(stream, ctx, eval_lock, stop).await {
                                warn!("connection error: {e}");
                            }
                        });
                    }
                    Err(e) => warn!("accept error: {e}"),
                }
            }
            () = stop.notified() => {
                info!("stop requested — shutting down");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT — shutting down");
                break;
            }
            _ = sigterm.recv() => {
                info!("SIGTERM — shutting down");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&path);

    // Persist the session image on clean shutdown.
    if let Some(ref img_path) = img {
        let source = ma_zscheme::dump_env_source(&ma_zscheme::get_env());
        match std::fs::write(img_path, source) {
            Ok(()) => info!(img = %img_path.display(), "session image saved"),
            Err(e) => warn!(img = %img_path.display(), "cannot save session image: {e}"),
        }
    }
    Ok(())
}

/// Take over the socket: ask a live daemon to stop, then remove the file.
async fn claim_socket(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if let Ok(mut stream) = UnixStream::connect(path).await {
        info!("asking the running daemon to stop");
        write_frame(&mut stream, &Request::Stop).await?;
        while let Some(resp) = read_frame::<_, Response>(&mut stream).await? {
            if matches!(resp, Response::Stopping) {
                break;
            }
        }
        // Wait for the old daemon to release the socket.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if !path.exists() {
                return Ok(());
            }
        }
        if UnixStream::connect(path).await.is_ok() {
            return Err(anyhow!(
                "running daemon did not release {} — kill it manually",
                path.display()
            ));
        }
    }
    std::fs::remove_file(path)
        .with_context(|| format!("cannot remove stale socket {}", path.display()))?;
    Ok(())
}

// ── Connection handling ────────────────────────────────────────────────────

async fn handle_conn(
    stream: UnixStream,
    ctx: Rc<CliCtx>,
    eval_lock: Rc<tokio::sync::Mutex<()>>,
    stop: Rc<Notify>,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    // Writer task: everything (results + streamed display events) goes
    // through one channel so frames never interleave mid-write.
    let (tx, mut rx) = unbounded_channel::<Response>();
    let writer_task = spawn_local(async move {
        while let Some(resp) = rx.recv().await {
            if write_frame(&mut writer, &resp).await.is_err() {
                break;
            }
        }
    });

    // Per-connection environment for isolated evals (lazy).
    let mut isolated_env: Option<ma_zscheme::Env> = None;

    while let Some(req) = read_frame::<_, Request>(&mut reader).await? {
        match req {
            Request::Hello { version } => {
                if version != env!("CARGO_PKG_VERSION") {
                    warn!(
                        client = %version,
                        daemon = env!("CARGO_PKG_VERSION"),
                        "client/daemon version mismatch"
                    );
                }
                let _ = tx.send(Response::HelloAck {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    did: ctx.our_did.clone(),
                });
            }
            Request::Ping => {
                let _ = tx.send(Response::Pong);
            }
            Request::Stop => {
                let _ = tx.send(Response::Stopping);
                stop.notify_one();
                break;
            }
            Request::Reset => {
                ma_zscheme::init_session_env();
                info!("session environment reset");
                let _ = tx.send(Response::ResetDone);
            }
            Request::DumpEnv => {
                let source = ma_zscheme::dump_env_source(&ma_zscheme::get_env());
                let _ = tx.send(Response::EnvDump { source });
            }
            Request::Eval {
                id,
                source,
                isolated,
            } => {
                let outcome = eval_request(
                    &ctx,
                    &eval_lock,
                    &tx,
                    id,
                    &source,
                    isolated,
                    &mut isolated_env,
                )
                .await;
                let _ = tx.send(Response::EvalResult { id, outcome });
            }
        }
    }

    drop(tx);
    let _ = writer_task.await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn eval_request(
    ctx: &Rc<CliCtx>,
    eval_lock: &tokio::sync::Mutex<()>,
    tx: &UnboundedSender<Response>,
    id: u64,
    source: &str,
    isolated: bool,
    isolated_env: &mut Option<ma_zscheme::Env>,
) -> Result<Option<String>, String> {
    // Serialize evaluations across all connections.
    let _guard = eval_lock.lock().await;

    // Route (display …) output to this client for the duration of the eval.
    let display_tx = tx.clone();
    ctx.set_display_sink(Some(Box::new(move |text: &str| {
        let _ = display_tx.send(Response::Display {
            id,
            text: text.to_string(),
        });
    })));

    let scheme_ctx: Ctx = ctx.clone();
    let result = if isolated {
        let env = isolated_env
            .get_or_insert_with(ma_zscheme::Env::new_root)
            .clone();
        ma_zscheme::eval_source_in(source, env, scheme_ctx).await
    } else {
        ma_zscheme::eval_source(source, scheme_ctx).await
    };

    ctx.set_display_sink(None);

    match result {
        Ok(SchemeVal::Nil) => Ok(None),
        Ok(val) => Ok(Some(val.display())),
        Err(e) => Err(format_scheme_err(&e)),
    }
}

/// Format a `SchemeErr` the same way the standalone REPL does.
pub fn format_scheme_err(e: &SchemeErr) -> String {
    match e {
        SchemeErr::Runtime(msg) => format!("error: {msg}"),
        SchemeErr::MaError(msg) => format!("ma error: {msg}"),
        SchemeErr::Undefined(sym) => format!("undefined: {sym}"),
        SchemeErr::Arity {
            name,
            expected,
            got,
        } => format!("{name}: expected {expected} args, got {got}"),
        SchemeErr::ParseError(msg) => format!("parse error: {msg}"),
    }
}
