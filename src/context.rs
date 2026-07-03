/// Evaluation context for the zscheme CLI interpreter.
///
/// `CliCtx` implements `ma_zscheme::SchemeCtx`, giving the evaluator access
/// to config, iroh transport, CID fetching, and terminal output.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{channel::oneshot, future::LocalBoxFuture};
use ma_core::{Did, IpfsGatewayResolver, Message, SigningKey, RPC_PROTOCOL_ID};
use ma_zscheme::{
    parse_actor_command, parse_dot_command, DotOp, DotRegistry, SchemeCtx, SchemeErr, SchemeVal,
};

// ── Context ────────────────────────────────────────────────────────────────

/// Shared evaluation context threaded through all recursive eval calls.
pub struct CliCtx {
    /// Dot-path registry — any DotRegistry backend (file, in-memory, IPFS, …).
    pub config: RefCell<Box<dyn DotRegistry>>,
    /// Our own DID (e.g. `did:ma:abc`)
    pub our_did: String,
    /// Ed25519 signing key bytes for outgoing messages.
    pub signing_key_bytes: [u8; 32],
    /// iroh endpoint for sending/receiving messages.
    pub endpoint: RefCell<Box<dyn ma_core::MaEndpoint>>,
    /// DID resolver for looking up actor endpoints.
    pub resolver: Rc<IpfsGatewayResolver>,
    /// Pending RPC reply senders keyed by message id.
    pub reply_senders: RefCell<HashMap<String, oneshot::Sender<Result<SchemeVal, String>>>>,

    /// RPC inbox for receiving replies.
    pub rpc_inbox: RefCell<ma_core::Inbox<Message>>,
    /// Kubo RPC base URL (e.g. `http://127.0.0.1:5001`).
    pub kubo_rpc_url: String,
    /// IPFS gateway fallback URL (e.g. `https://dweb.link`).
    pub gateway_url: String,
    /// reqwest client (reused across CID fetches).
    pub http: reqwest::Client,
}

/// Re-export the ma-zscheme Ctx type (Rc<dyn SchemeCtx>) for use in main.rs,
/// repl.rs, and executor.rs.
pub use ma_zscheme::Ctx;

// ── Constructor ────────────────────────────────────────────────────────────

impl CliCtx {
    pub fn new(
        config: Box<dyn DotRegistry>,
        our_did: String,
        signing_key_bytes: [u8; 32],
        endpoint: Box<dyn ma_core::MaEndpoint>,
        resolver: Rc<IpfsGatewayResolver>,
        rpc_inbox: ma_core::Inbox<Message>,
        kubo_rpc_url: String,
        gateway_url: String,
    ) -> Rc<Self> {
        Rc::new(Self {
            config: RefCell::new(config),
            our_did,
            signing_key_bytes,
            endpoint: RefCell::new(endpoint),
            resolver,
            reply_senders: RefCell::new(HashMap::new()),
            rpc_inbox: RefCell::new(rpc_inbox),
            kubo_rpc_url,
            gateway_url,
            http: reqwest::Client::new(),
        })
    }
}

// ── Non-trait methods ──────────────────────────────────────────────────────

impl CliCtx {
    /// Config read helper.
    #[allow(dead_code)]
    pub fn config_get(&self, path: &str) -> Option<String> {
        self.config.borrow().get(path)
    }

    /// Close the iroh endpoint gracefully.
    pub async fn close(&self) {
        self.endpoint.borrow_mut().close().await;
    }

    /// Drain the RPC inbox and route replies to waiting `oneshot` senders.
    /// Call periodically from the poll loop in main.rs.
    pub fn poll_rpc_replies(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let messages: Vec<Message> = self.rpc_inbox.borrow_mut().drain(now);

        for msg in messages {
            let reply_to = match &msg.reply_to {
                Some(id) => id.clone(),
                None => continue,
            };
            let payload = msg.payload();
            let reply_result = decode_rpc_reply(&payload);
            if let Some(sender) = self.reply_senders.borrow_mut().remove(&reply_to) {
                let _ = sender.send(reply_result);
            }
        }
    }
}

// ── SchemeCtx implementation ───────────────────────────────────────────────

impl SchemeCtx for CliCtx {
    // ── Sync ─────────────────────────────────────────────────────────────

    fn eval_dot(&self, command: &str) -> Result<SchemeVal, SchemeErr> {
        let (path, op) = parse_dot_command(command)
            .ok_or_else(|| SchemeErr::MaError(format!("bad dot command: {command}")))?;

        match op {
            DotOp::Get => {
                if let Some(val) = self.config.borrow().get(&path) {
                    Ok(SchemeVal::Str(val))
                } else {
                    let pairs = self.config.borrow().list(&path);
                    if pairs.is_empty() {
                        Err(SchemeErr::MaError(format!("no value at .{path}")))
                    } else {
                        Ok(SchemeVal::List(
                            pairs.into_iter().map(|(k, _)| SchemeVal::Str(k)).collect(),
                        ))
                    }
                }
            }
            DotOp::Set(val) => {
                self.config.borrow_mut().set(&path, &val);
                Ok(SchemeVal::Nil)
            }
            DotOp::Delete => {
                self.config.borrow_mut().delete_subtree(&path);
                Ok(SchemeVal::Nil)
            }
            DotOp::Meta { verb, args } => {
                tracing::warn!("dot meta-verb .{path}!{verb} {args}: not yet supported in CLI");
                Ok(SchemeVal::Nil)
            }
        }
    }

    fn display_output(&self, text: &str) {
        print!("{text}");
    }

    fn resolve_target(&self, raw: &str) -> Result<String, String> {
        self.config.borrow().resolve_target(raw)
    }

    fn register_reply_sender(
        &self,
        msg_id: String,
        sender: oneshot::Sender<Result<SchemeVal, String>>,
    ) {
        self.reply_senders.borrow_mut().insert(msg_id, sender);
    }

    // ── Async ─────────────────────────────────────────────────────────────

    fn fetch_cid<'a>(&'a self, cid: &'a str) -> LocalBoxFuture<'a, Result<String, String>> {
        let kubo_url = format!(
            "{}/api/v0/cat?arg={}",
            self.kubo_rpc_url.trim_end_matches('/'),
            cid
        );
        let gw_url = format!("{}/ipfs/{}", self.gateway_url.trim_end_matches('/'), cid);
        let http = self.http.clone();
        Box::pin(async move {
            match http.post(&kubo_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return resp.text().await.map_err(|e| e.to_string());
                }
                _ => {}
            }
            http.get(&gw_url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .text()
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn eval_actor<'a>(
        &'a self,
        command: &'a str,
    ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move {
            let effective = if command.starts_with('@') || command.starts_with("did:") {
                command.to_string()
            } else {
                format!("@{command}")
            };

            let cfg = self.config.borrow();
            let (target, verb, str_args) =
                parse_actor_command(&effective, &**cfg).map_err(SchemeErr::MaError)?;
            let scheme_args: Vec<SchemeVal> = str_args.into_iter().map(SchemeVal::Str).collect();

            let msg_id = self
                .send_rpc(&target, &verb, &scheme_args)
                .await
                .map_err(SchemeErr::MaError)?;

            let (sender, receiver) = oneshot::channel::<Result<SchemeVal, String>>();
            self.register_reply_sender(msg_id, sender);

            match receiver.await {
                Ok(Ok(val)) => Ok(val),
                Ok(Err(e)) => Err(SchemeErr::MaError(e)),
                Err(_) => Err(SchemeErr::MaError(
                    "RPC reply channel cancelled".to_string(),
                )),
            }
        })
    }

    fn eval_actor_with_vals<'a>(
        &'a self,
        actor: &'a str,
        args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move {
            let effective = if actor.starts_with('@') || actor.starts_with("did:") {
                actor.to_string()
            } else {
                format!("@{actor}")
            };

            let cfg = self.config.borrow();
            // Parse target+verb from the actor string; ignore any string args
            // (the SchemeVal args are passed directly to send_rpc).
            let (target, verb, _) =
                parse_actor_command(&effective, &**cfg).map_err(SchemeErr::MaError)?;

            let msg_id = self
                .send_rpc(&target, &verb, args)
                .await
                .map_err(SchemeErr::MaError)?;

            let (sender, receiver) = oneshot::channel::<Result<SchemeVal, String>>();
            self.register_reply_sender(msg_id, sender);

            match receiver.await {
                Ok(Ok(val)) => Ok(val),
                Ok(Err(e)) => Err(SchemeErr::MaError(e)),
                Err(_) => Err(SchemeErr::MaError(
                    "RPC reply channel cancelled".to_string(),
                )),
            }
        })
    }

    fn send_rpc<'a>(
        &'a self,
        target: &'a str,
        verb: &'a str,
        args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        let did = match Did::try_from(target) {
            Ok(d) => d,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let signing_key = match SigningKey::from_private_key_bytes(did, self.signing_key_bytes) {
            Ok(k) => k,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let atom = if verb.starts_with(':') {
            verb.to_string()
        } else {
            format!(":{verb}")
        };
        let cbor_val = if args.is_empty() {
            ciborium::Value::Text(atom)
        } else {
            let mut items = Vec::with_capacity(1 + args.len());
            items.push(ciborium::Value::Text(atom));
            for a in args {
                items.push(scheme_val_to_cbor(a));
            }
            ciborium::Value::Array(items)
        };
        let mut body = Vec::new();
        if let Err(e) = ciborium::ser::into_writer(&cbor_val, &mut body) {
            return Box::pin(futures::future::ready(Err(e.to_string())));
        }
        let msg = match ma_core::Message::new(
            &self.our_did,
            target,
            ma_core::MESSAGE_TYPE_RPC,
            ma_core::CONTENT_TYPE_TERM,
            &body,
            &signing_key,
        ) {
            Ok(m) => m,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let msg_id = msg.id.clone();
        let resolver = self.resolver.clone();
        let target_owned = target.to_string();
        Box::pin(async move {
            let mut outbox = self
                .endpoint
                .borrow()
                .outbox(resolver.as_ref(), &target_owned, RPC_PROTOCOL_ID)
                .await
                .map_err(|e| e.to_string())?;
            outbox.send(&msg).await.map_err(|e| e.to_string())?;
            Ok(msg_id)
        })
    }

    fn send_text<'a>(
        &'a self,
        target: &'a str,
        body: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        use ma_core::{INBOX_PROTOCOL_ID, MESSAGE_TYPE_MESSAGE};
        let did = match Did::try_from(target) {
            Ok(d) => d,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let signing_key = match SigningKey::from_private_key_bytes(did, self.signing_key_bytes) {
            Ok(k) => k,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let msg = match ma_core::Message::new(
            &self.our_did,
            target,
            MESSAGE_TYPE_MESSAGE,
            "text/plain",
            body.as_bytes(),
            &signing_key,
        ) {
            Ok(m) => m,
            Err(e) => return Box::pin(futures::future::ready(Err(e.to_string()))),
        };
        let msg_id = msg.id.clone();
        let resolver = self.resolver.clone();
        let target_owned = target.to_string();
        Box::pin(async move {
            let mut outbox = self
                .endpoint
                .borrow()
                .outbox(resolver.as_ref(), &target_owned, INBOX_PROTOCOL_ID)
                .await
                .map_err(|e| e.to_string())?;
            outbox.send(&msg).await.map_err(|e| e.to_string())?;
            Ok(msg_id)
        })
    }
}

// ── CBOR ↔ SchemeVal conversion ──────────────────────────────────────────

fn scheme_val_to_cbor(v: &SchemeVal) -> ciborium::Value {
    use ciborium::Value as V;
    match v {
        SchemeVal::Str(s) => V::Text(s.clone()),
        SchemeVal::Int(n) => V::Integer(ciborium::value::Integer::from(*n)),
        SchemeVal::Float(f) => V::Float(*f),
        SchemeVal::Bool(b) => V::Bool(*b),
        SchemeVal::Nil => V::Null,
        SchemeVal::List(items) => V::Array(items.iter().map(scheme_val_to_cbor).collect()),
        // Lambdas and builtins can't be serialised — use their display string.
        other => V::Text(other.display()),
    }
}

fn cbor_to_scheme_val(v: &ciborium::Value) -> SchemeVal {
    use ciborium::Value as V;
    match v {
        V::Text(s) => SchemeVal::Str(s.clone()),
        V::Integer(n) => SchemeVal::Int(i128::from(*n) as i64),
        V::Float(f) => SchemeVal::Float(*f),
        V::Bool(b) => SchemeVal::Bool(*b),
        V::Null => SchemeVal::Nil,
        V::Array(items) => SchemeVal::List(items.iter().map(cbor_to_scheme_val).collect()),
        V::Map(pairs) => SchemeVal::List(
            pairs
                .iter()
                .map(|(k, v)| SchemeVal::List(vec![cbor_to_scheme_val(k), cbor_to_scheme_val(v)]))
                .collect(),
        ),
        V::Tag(_, inner) => cbor_to_scheme_val(inner),
        _ => SchemeVal::Str(format!("{v:?}")),
    }
}

fn decode_rpc_reply(payload: &[u8]) -> Result<SchemeVal, String> {
    use ciborium::Value as V;
    let val: V = match ciborium::de::from_reader(payload) {
        Ok(v) => v,
        Err(_) => return Ok(SchemeVal::Str(String::from_utf8_lossy(payload).to_string())),
    };
    match &val {
        V::Text(s) if s == ":ok" => Ok(SchemeVal::Nil),
        V::Array(items) => match (items.first(), items.get(1)) {
            (Some(V::Text(verb)), _) if verb == ":ok" => match items.get(1) {
                Some(v) => Ok(cbor_to_scheme_val(v)),
                None => Ok(SchemeVal::Nil),
            },
            (Some(V::Text(verb)), _) if verb == ":error" => {
                let reason = items
                    .get(1)
                    .and_then(|v| {
                        if let V::Text(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| String::from_utf8_lossy(payload).to_string());
                Err(reason)
            }
            _ => Ok(cbor_to_scheme_val(&val)),
        },
        _ => Ok(cbor_to_scheme_val(&val)),
    }
}
