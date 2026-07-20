mod client;
mod context;
mod daemon;
mod executor;
mod ipc;
mod repl;
mod scheme;
mod transport;

use std::rc::Rc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use ma_core::config::{Config, MaArgs, SecretBundle};
use ma_core::{IpfsGatewayResolver, RPC_PROTOCOL_ID};
use tokio::task::spawn_local;
use tracing::{info, warn};
use zeroize::Zeroize;

use crate::context::{CliCtx, CliCtxInit, Ctx};
use crate::scheme::init_session_env;
use ma_zscheme_yaml::SchemeConfig;

const ZSCHEME_SLUG: &str = "zscheme";
const DEFAULT_GATEWAY_URL: &str = "https://dweb.link";

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(name = "zscheme")]
#[command(about = "zscheme — a Scheme interpreter for the ma actor network")]
struct Cli {
    #[command(flatten)]
    ma: MaArgs,

    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Script file to execute. If omitted, starts the interactive REPL.
    /// Both run against the backend daemon (auto-spawned if needed).
    script: Option<std::path::PathBuf>,

    /// IPFS gateway URL (fallback when local Kubo is unavailable).
    #[arg(long, default_value = DEFAULT_GATEWAY_URL, env = "ZSCHEME_GATEWAY")]
    gateway: String,

    /// How often to drain the iroh RPC inbox for actor-call replies (milliseconds).
    /// Lower values reduce latency for (@actor verb) and (rpc-send …) calls.
    #[arg(long, default_value_t = 50, env = "ZSCHEME_RPC_POLL_MS")]
    rpc_poll_ms: u64,

    /// Use a fresh per-connection Scheme environment instead of the shared
    /// daemon session environment.
    #[arg(long)]
    isolated: bool,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Run the backend daemon: own the iroh endpoint and evaluate Scheme
    /// submitted by clients. Replaces a running daemon (fresh environment).
    Daemon {
        /// Session-image file: evaluated into the environment at startup
        /// (if it exists) and rewritten on clean shutdown.
        #[arg(long)]
        img: Option<std::path::PathBuf>,
    },
    /// Stop the running backend daemon.
    Stop,
    /// Reset the daemon's shared session environment (drop all defines).
    Reset,
    /// Save the daemon's session environment as Scheme source.
    Save {
        /// Output file. Writes to stdout when omitted.
        file: Option<std::path::PathBuf>,
    },
    /// Run fully in-process (own iroh endpoint, no daemon). Only one
    /// standalone/daemon process per identity may run at a time.
    Standalone {
        /// Script file to execute. If omitted, starts the interactive REPL.
        script: Option<std::path::PathBuf>,
    },
}

// ── Entry point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle --gen-headless-config
    if cli.ma.gen_headless_config {
        Config::gen_headless(&cli.ma, ZSCHEME_SLUG)?;
        return Ok(());
    }

    // Set up stderr-only tracing (stdout is reserved for script output).
    // Note: --log-level-stdout from MaArgs is a no-op in zscheme; logging
    // is controlled via RUST_LOG or the YAML log_level / log_file settings.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Some(Cmd::Stop) = cli.cmd {
        return client::stop().await;
    }
    if let Some(Cmd::Reset) = cli.cmd {
        return client::reset().await;
    }
    if let Some(Cmd::Save { file }) = cli.cmd {
        return client::save(file).await;
    }

    // Default mode: thin client — no secret bundle, no iroh endpoint.
    // The daemon (auto-spawned if needed) owns the single endpoint.
    let (is_daemon, img, script) = match cli.cmd {
        None => {
            return client::run(cli.script.clone(), cli.isolated).await;
        }
        Some(Cmd::Daemon { img }) => (true, img, None),
        Some(Cmd::Standalone { script }) => (false, None, script),
        Some(Cmd::Stop | Cmd::Reset | Cmd::Save { .. }) => unreachable!(),
    };

    let bundle_path_check = {
        let cfg_tmp = Config::from_args(&cli.ma, ZSCHEME_SLUG)?;
        cfg_tmp.effective_secret_bundle()?
    };

    let core_config = if bundle_path_check.exists() {
        Config::from_args(&cli.ma, ZSCHEME_SLUG)?
    } else {
        warn!("No zscheme identity found — generating a new one.");
        Config::gen_headless(&cli.ma, ZSCHEME_SLUG)?;
        Config::from_args(&cli.ma, ZSCHEME_SLUG)?
    };

    let mut secrets = load_secret_bundle(&core_config)?;

    // ── iroh endpoint ───────────────────────────────────────────────────────
    let mut endpoint = ma_core::new_ma_endpoint(secrets.iroh_secret_key, true).await?;
    let rpc_inbox = endpoint.service(RPC_PROTOCOL_ID);

    // ── DID document ────────────────────────────────────────────────────────
    let ma_ext = endpoint.ma_extension().kind("agent");
    let our_document = secrets
        .build_document(ma_ext)
        .context("failed to build DID document")?;
    let our_did = our_document.id.clone();
    info!(did = %our_did, "zscheme identity ready");

    // Box the endpoint now that service() and ma_extension() are done.
    // (new_ma_endpoint already returns Box<dyn MaEndpoint>; no re-boxing needed.)

    // Zeroize key material we no longer need.
    secrets.ipns_secret_key.zeroize();

    // ── Scheme data config ──────────────────────────────────────────────────
    let data_path = SchemeConfig::default_path()?;
    let scheme_config = SchemeConfig::load(&data_path);

    // ── Build CliCtx ────────────────────────────────────────────────────────
    let resolver = Rc::new(IpfsGatewayResolver::default());
    let signing_key_bytes = secrets.did_signing_key;

    let ctx = CliCtx::new(CliCtxInit {
        config: Box::new(scheme_config),
        our_did: our_did.clone(),
        signing_key_bytes,
        endpoint,
        resolver,
        rpc_inbox,
        kubo_rpc_url: core_config.kubo_rpc_url.clone(),
        gateway_url: cli.gateway.clone(),
    });

    // Zeroize signing key copy from secrets after it has been stored in ctx.
    secrets.did_signing_key.zeroize();

    // ── Run in LocalSet (required for Rc<…> + LocalBoxFuture) ─────────────
    let local = tokio::task::LocalSet::new();
    if is_daemon {
        local
            .run_until(daemon_main(ctx, img, cli.rpc_poll_ms))
            .await
    } else {
        local
            .run_until(async_main(ctx, script, cli.rpc_poll_ms))
            .await
    }
}

/// Backend daemon mode: full identity + endpoint, serving frontend clients.
async fn daemon_main(
    ctx: std::rc::Rc<CliCtx>,
    img: Option<std::path::PathBuf>,
    poll_ms: u64,
) -> Result<()> {
    init_session_env();
    spawn_rpc_poll_loop(ctx.clone(), poll_ms);
    let result = daemon::run(ctx.clone(), img).await;
    ctx.close().await;
    result
}

async fn async_main(
    ctx: std::rc::Rc<CliCtx>, // Rc<CliCtx> for poll_rpc_replies access
    script: Option<std::path::PathBuf>,
    poll_ms: u64,
) -> Result<()> {
    // Initialise session environment.
    init_session_env();

    // Coerce to Ctx (= Rc<dyn SchemeCtx>) for the evaluator.
    let scheme_ctx: Ctx = ctx.clone();

    // Start the RPC reply poll loop.
    spawn_rpc_poll_loop(ctx.clone(), poll_ms);

    // Execute script or REPL, then close the endpoint cleanly.
    let result = if let Some(ref path) = script {
        executor::run_file(path, scheme_ctx).await
    } else {
        repl::run_repl(repl::LocalEval(scheme_ctx)).await
    };
    ctx.close().await;
    result
}

/// Spawn the periodic RPC-inbox drain that routes replies to waiting calls.
fn spawn_rpc_poll_loop(ctx: std::rc::Rc<CliCtx>, poll_ms: u64) {
    spawn_local(async move {
        let interval = Duration::from_millis(poll_ms);
        loop {
            tokio::time::sleep(interval).await;
            ctx.poll_rpc_replies();
        }
    });
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn load_secret_bundle(config: &Config) -> Result<SecretBundle> {
    let passphrase = config
        .secret_bundle_passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("secret_bundle_passphrase is required (set MA_SECRET_BUNDLE_PASSPHRASE or add it to {ZSCHEME_SLUG}.yaml)"))?;
    let bundle_path = config.effective_secret_bundle()?;
    SecretBundle::load(&bundle_path, passphrase).with_context(|| {
        format!(
            "failed to load secret bundle from {}",
            bundle_path.display()
        )
    })
}
