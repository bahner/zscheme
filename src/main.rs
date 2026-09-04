mod client;
mod context;
mod daemon;
mod executor;
mod ipc;
mod repl;
mod scheme;
mod transport;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use ma_core::config::{Config, MaArgs, SecretBundle};
use ma_core::ipfs::{DidDocumentPublishOptions, IpfsDidPublisher, RemotePinOptions};
use ma_core::{IpfsGatewayResolver, MaExtension, INBOX_PROTOCOL_ID};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};
use tokio::task::spawn_local;
use tracing::{info, warn};
use zeroize::{Zeroize, Zeroizing};

use crate::context::{CliCtx, CliCtxInit, Ctx};
use crate::scheme::init_session_env;
use ma_zscheme_yaml::SchemeConfig;

const ZSCHEME_SLUG: &str = "zscheme";
const DEFAULT_GATEWAY_URL: &str = "https://dweb.link";
const DID_REPUBLISH_INTERVAL: Duration = Duration::from_secs(60 * 60);

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

    /// How often to drain the iroh inbox for actor-call replies (milliseconds).
    /// Lower values reduce latency for (@actor verb) and (rpc-send …) calls.
    #[arg(long, default_value_t = 50, env = "ZSCHEME_POLL_MS")]
    poll_ms: u64,

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
    let publication_secrets = is_daemon.then(|| secrets.clone());
    let resolver = Arc::new(IpfsGatewayResolver::local_first(&cli.gateway));

    // ── iroh endpoint ───────────────────────────────────────────────────────
    let mut endpoint = ma_core::new_ma_endpoint(
        secrets.iroh_secret_key,
        secrets.encryption_key()?,
        resolver.clone(),
        true,
    )
    .await?;
    let inbox = endpoint.service(INBOX_PROTOCOL_ID);

    // ── DID document ────────────────────────────────────────────────────────
    let ma_ext = endpoint.ma_extension().kind("agent");
    let our_document = secrets
        .build_document(ma_ext.clone())
        .context("failed to build DID document")?;
    let our_did = our_document.id.clone();
    info!(did = %our_did, "zscheme identity ready");

    publish_did_document(&core_config, &our_document, &secrets.ipns_secret_key).await?;

    // Box the endpoint now that service() and ma_extension() are done.
    // (new_ma_endpoint already returns Box<dyn MaEndpoint>; no re-boxing needed.)

    // Zeroize key material we no longer need. Daemon mode retains its separate
    // publication bundle so it can re-sign the current document periodically.
    secrets.ipns_secret_key.zeroize();

    // ── Scheme data config ──────────────────────────────────────────────────
    let data_path = SchemeConfig::default_path()?;
    let scheme_config = SchemeConfig::load(&data_path);

    // ── Build CliCtx ────────────────────────────────────────────────────────
    // Local-first pool: localhost gateway, then --gateway, then public fallbacks.
    let signing_key_bytes = secrets.did_signing_key;

    let ctx = CliCtx::new(CliCtxInit {
        config: Box::new(scheme_config),
        our_did: our_did.clone(),
        signing_key_bytes,
        endpoint,
        resolver,
        inbox,
        kubo_rpc_url: core_config.kubo_rpc_url.clone(),
    });

    // Zeroize signing key copy from secrets after it has been stored in ctx.
    secrets.did_signing_key.zeroize();

    // ── Run in LocalSet (required for Rc<…> + LocalBoxFuture) ─────────────
    let local = tokio::task::LocalSet::new();
    if is_daemon {
        spawn_periodic_did_publish(
            core_config.clone(),
            ma_ext,
            publication_secrets.expect("daemon publication bundle"),
        );
        local.run_until(daemon_main(ctx, img, cli.poll_ms)).await
    } else {
        local.run_until(async_main(ctx, script, cli.poll_ms)).await
    }
}

/// Backend daemon mode: full identity + endpoint, serving frontend clients.
async fn daemon_main(
    ctx: std::rc::Rc<CliCtx>,
    img: Option<std::path::PathBuf>,
    poll_ms: u64,
) -> Result<()> {
    init_session_env();
    spawn_poll_loop(ctx.clone(), poll_ms);
    let result = daemon::run(ctx.clone(), img).await;
    ctx.close().await;
    result
}

async fn async_main(
    ctx: std::rc::Rc<CliCtx>, // Rc<CliCtx> for poll_replies access
    script: Option<std::path::PathBuf>,
    poll_ms: u64,
) -> Result<()> {
    // Initialise session environment.
    init_session_env();

    // Coerce to Ctx (= Rc<dyn SchemeCtx>) for the evaluator.
    let scheme_ctx: Ctx = ctx.clone();

    // Start the reply poll loop.
    spawn_poll_loop(ctx.clone(), poll_ms);

    // Execute script or REPL, then close the endpoint cleanly.
    let result = if let Some(ref path) = script {
        executor::run_file(path, scheme_ctx).await
    } else {
        repl::run_repl(repl::LocalEval(scheme_ctx)).await
    };
    ctx.close().await;
    result
}

/// Spawn the periodic inbox drain that routes replies to waiting calls.
fn spawn_poll_loop(ctx: std::rc::Rc<CliCtx>, poll_ms: u64) {
    spawn_local(async move {
        let interval = Duration::from_millis(poll_ms);
        loop {
            tokio::time::sleep(interval).await;
            ctx.poll_replies();
        }
    });
}

/// Publish the current DID document and retain one named archive pin per UTC day.
async fn publish_did_document(
    config: &Config,
    document: &ma_core::Document,
    ipns_secret_key: &[u8; 32],
) -> Result<()> {
    document
        .validate()
        .context("DID document failed validation before publication")?;
    document
        .verify()
        .context("DID document proof failed verification before publication")?;
    let document_cbor = document
        .encode()
        .context("failed to encode DID document for publication")?;
    let publisher = IpfsDidPublisher::new(&config.kubo_rpc_url)
        .with_context(|| format!("invalid kubo_rpc_url: {}", config.kubo_rpc_url))?;
    publisher
        .wait_until_ready(10)
        .await
        .context("Kubo RPC is not reachable for DID publication")?;

    let daily_pin_name = daily_did_pin_name(&config.slug, &document.id);
    let remote_pin = config
        .remote_pin_config_with_default_name(&daily_pin_name)
        .context("remote DID pinning is misconfigured")?
        .map(|remote| RemotePinOptions {
            service: remote.service,
            // DID archive naming is fixed so repeated publishes retain one
            // document per day locally and on the configured remote service.
            name: daily_pin_name.clone(),
        });
    let options = DidDocumentPublishOptions {
        key_parts: vec!["zscheme".to_string(), config.slug.clone()],
        remote_pin,
        overwrite: true,
        ..DidDocumentPublishOptions::default()
    };

    let publication = publisher
        .publish_document(
            document_cbor,
            Zeroizing::new(ipns_secret_key.to_vec()),
            options,
        )
        .await
        .context("failed to publish DID document")?;
    info!(
        did = %document.id,
        cid = %publication.cid,
        key = %publication.key_name,
        archive_pin = %daily_pin_name,
        "zscheme DID document published"
    );
    Ok(())
}

fn spawn_periodic_did_publish(
    config: Config,
    ma_ext: MaExtension,
    publication_secrets: SecretBundle,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DID_REPUBLISH_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let result = publication_secrets
                .build_document(ma_ext.clone())
                .map_err(anyhow::Error::from);
            match result {
                Ok(document) => {
                    if let Err(error) = publish_did_document(
                        &config,
                        &document,
                        &publication_secrets.ipns_secret_key,
                    )
                    .await
                    {
                        warn!(error = %format!("{error:#}"), "periodic zscheme DID publication failed");
                    }
                }
                Err(error) => {
                    warn!(error = %format!("{error:#}"), "periodic zscheme DID document build failed");
                }
            }
        }
    });
}

fn daily_did_pin_name(slug: &str, did: &str) -> String {
    let date = time::OffsetDateTime::now_utc().date();
    let digest = blake3::hash(did.as_bytes()).to_hex();
    format!(
        "ma-zscheme-{slug}-{}-{:04}-{:02}-{:02}",
        &digest[..16],
        date.year(),
        u8::from(date.month()),
        date.day(),
    )
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn load_secret_bundle(config: &Config) -> Result<SecretBundle> {
    let passphrase = config
        .secret_bundle_passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("secret_bundle_passphrase is required (set MA_SECRET_BUNDLE_PASSPHRASE or add it to {ZSCHEME_SLUG}.yaml)"))?;
    let bundle_path = config.effective_secret_bundle()?;
    let mut bundle = SecretBundle::load(&bundle_path, passphrase).with_context(|| {
        format!(
            "failed to load secret bundle from {}",
            bundle_path.display()
        )
    })?;
    let canonical_created_at = canonicalise_created_at(&bundle.created_at)?;
    if canonical_created_at != bundle.created_at {
        bundle.created_at = canonical_created_at;
        bundle.save(&bundle_path, passphrase).with_context(|| {
            format!(
                "failed to persist migrated secret bundle to {}",
                bundle_path.display()
            )
        })?;
    }
    Ok(bundle)
}

fn canonicalise_created_at(value: &str) -> Result<String> {
    OffsetDateTime::parse(value, &Rfc3339)
        .context("invalid secret bundle created_at")?
        .to_offset(UtcOffset::UTC)
        .replace_nanosecond(0)
        .context("invalid secret bundle created_at")?
        .format(&Rfc3339)
        .context("format secret bundle created_at")
}

#[cfg(test)]
mod did_document_tests {
    use super::canonicalise_created_at;

    #[test]
    fn canonicalises_legacy_fractional_created_at() {
        assert_eq!(
            canonicalise_created_at("2026-07-19T19:45:24.489Z").unwrap(),
            "2026-07-19T19:45:24Z"
        );
    }
}
