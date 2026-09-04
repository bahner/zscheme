/// Evaluation context for the zscheme CLI interpreter.
///
/// `CliCtx` implements `ma_zscheme::SchemeCtx`, giving the evaluator access
/// to config, iroh transport, CID fetching, and terminal output.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{channel::oneshot, future::LocalBoxFuture};
use ma_core::{Did, IpfsGatewayResolver, Message, SigningKey, INBOX_PROTOCOL_ID};
use ma_zscheme::{
    parse_actor_command, parse_dot_command, DotOp, DotRegistry, SchemeCtx, SchemeErr, SchemeVal,
};

// ── Context ────────────────────────────────────────────────────────────────
/// Redirect target for `(display …)` output (daemon mode).
pub type DisplaySink = Box<dyn Fn(&str)>;
/// Shared evaluation context threaded through all recursive eval calls.
pub struct CliCtx {
    /// Path registry — any `DotRegistry` backend (file, in-memory, IPFS, ...).
    pub config: RefCell<Box<dyn DotRegistry>>,
    /// Our own DID (e.g. `did:ma:abc`)
    pub our_did: String,
    /// Ed25519 signing key bytes for outgoing messages.
    pub signing_key_bytes: [u8; 32],
    /// iroh endpoint for sending/receiving messages.
    pub endpoint: tokio::sync::Mutex<Box<dyn ma_core::MaEndpoint>>,
    /// DID resolver for looking up actor endpoints.
    pub resolver: Arc<IpfsGatewayResolver>,
    /// Pending reply senders keyed by message id.
    pub reply_senders: RefCell<HashMap<String, oneshot::Sender<Result<SchemeVal, String>>>>,

    /// Inbox for receiving replies.
    pub inbox: RefCell<ma_core::Inbox<Message>>,
    /// Kubo RPC base URL (e.g. `http://127.0.0.1:5001`).
    pub kubo_rpc_url: String,
    /// reqwest client (reused for local Kubo cat calls).
    pub http: reqwest::Client,
    /// Optional display redirect. When set (daemon mode), `(display …)`
    /// output is routed here instead of stdout.
    pub display_sink: RefCell<Option<DisplaySink>>,
}

/// Inputs needed to construct a [`CliCtx`].
pub struct CliCtxInit {
    /// Path registry backend.
    pub config: Box<dyn DotRegistry>,
    /// Our own DID.
    pub our_did: String,
    /// Ed25519 signing key bytes for outgoing messages.
    pub signing_key_bytes: [u8; 32],
    /// iroh endpoint for transport.
    pub endpoint: Box<dyn ma_core::MaEndpoint>,
    /// DID resolver for target lookup.
    pub resolver: Arc<IpfsGatewayResolver>,
    /// Inbox for receiving replies.
    pub inbox: ma_core::Inbox<Message>,
    /// Kubo RPC base URL.
    pub kubo_rpc_url: String,
}

/// Re-export the ma-zscheme Ctx type (Rc<dyn SchemeCtx>) for use in main.rs,
/// repl.rs, and executor.rs.
pub use ma_zscheme::Ctx;

// ── Constructor ────────────────────────────────────────────────────────────

impl CliCtx {
    /// Build a shared CLI evaluation context.
    #[must_use]
    pub fn new(init: CliCtxInit) -> Rc<Self> {
        Rc::new(Self {
            config: RefCell::new(init.config),
            our_did: init.our_did,
            signing_key_bytes: init.signing_key_bytes,
            endpoint: tokio::sync::Mutex::new(init.endpoint),
            resolver: init.resolver,
            reply_senders: RefCell::new(HashMap::new()),
            inbox: RefCell::new(init.inbox),
            kubo_rpc_url: init.kubo_rpc_url,
            http: reqwest::Client::new(),
            display_sink: RefCell::new(None),
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
        self.endpoint.lock().await.close().await;
    }

    /// Redirect `(display …)` output to `sink` (daemon mode), or restore
    /// stdout output with `None`.
    pub fn set_display_sink(&self, sink: Option<DisplaySink>) {
        *self.display_sink.borrow_mut() = sink;
    }

    fn sender_url(&self, fragment: &str) -> Result<String, String> {
        let did = Did::try_from(self.our_did.as_str()).map_err(|error| error.to_string())?;
        did.with_fragment(fragment)
            .map(|url| url.id())
            .map_err(|error| error.to_string())
    }

    /// Build a `SigningKey` and return it together with the sender DID-URL.
    fn build_signing_key(&self, fragment: &str) -> Result<(String, SigningKey), String> {
        let sender = self.sender_url(fragment)?;
        let signing_did = Did::try_from(sender.as_str()).map_err(|e| e.to_string())?;
        let signing_key = SigningKey::from_private_key_bytes(signing_did, self.signing_key_bytes)
            .map_err(|e| e.to_string())?;
        Ok((sender, signing_key))
    }

    /// Send an actor message to `target#verb(args)` and await the reply.
    async fn do_actor_call(
        &self,
        target: &str,
        verb: &str,
        args: &[SchemeVal],
    ) -> Result<SchemeVal, SchemeErr> {
        let msg_id = self
            .send_actor(target, verb, args)
            .await
            .map_err(SchemeErr::MaError)?;
        let (sender, receiver) = oneshot::channel::<Result<SchemeVal, String>>();
        self.register_reply_sender(msg_id, sender);
        match receiver.await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(e)) => Err(SchemeErr::MaError(e)),
            Err(_) => Err(SchemeErr::MaError("reply channel cancelled".to_string())),
        }
    }

    /// Drain the inbox and route replies to waiting `oneshot` senders.
    /// Call periodically from the poll loop in main.rs.
    pub fn poll_replies(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let messages: Vec<Message> = self.inbox.borrow_mut().drain(now);

        for msg in messages {
            let reply_to = match &msg.reply_to {
                Some(id) => id.clone(),
                None => continue,
            };
            let payload = msg.payload();
            let reply_result = decode_reply(&payload);
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
            .ok_or_else(|| SchemeErr::MaError(format!("bad path command: {command}")))?;

        match op {
            DotOp::Get => {
                if let Some(val) = self.config.borrow().get(&path) {
                    Ok(SchemeVal::Str(val))
                } else {
                    let pairs = self.config.borrow().list(&path);
                    if pairs.is_empty() {
                        Err(SchemeErr::MaError(format!(
                            "no value at .{}",
                            path.replace('/', ".")
                        )))
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
                tracing::warn!(
                    "path meta-verb .{}!{verb} {args}: not yet supported in CLI",
                    path.replace('/', ".")
                );
                Ok(SchemeVal::Nil)
            }
        }
    }

    fn display_output(&self, text: &str) {
        if let Some(sink) = self.display_sink.borrow().as_ref() {
            sink(text);
        } else {
            print!("{text}");
        }
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

    fn random_bytes(&self, len: usize) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0; len];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    // ── Async ─────────────────────────────────────────────────────────────

    fn fetch_path<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<String, String>> {
        let http = self.http.clone();
        let resolver = self.resolver.clone();
        let kubo_rpc_url = self.kubo_rpc_url.clone();
        let path = path.to_string();
        Box::pin(async move {
            if let Some(resp) = try_kubo_cat(&http, &kubo_rpc_url, &path).await {
                return resp.text().await.map_err(|e| e.to_string());
            }
            resolver
                .pool()
                .fetch(&path, None, |body| {
                    String::from_utf8(body.to_vec()).map_err(|e| e.to_string())
                })
                .await
        })
    }

    fn fetch_bytes<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<Vec<u8>, String>> {
        let http = self.http.clone();
        let resolver = self.resolver.clone();
        let kubo_rpc_url = self.kubo_rpc_url.clone();
        let path = path.to_string();
        Box::pin(async move {
            if let Some(resp) = try_kubo_cat(&http, &kubo_rpc_url, &path).await {
                return resp
                    .bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| e.to_string());
            }
            resolver.pool().fetch_bytes(&path, None).await
        })
    }

    fn resolve_ipns<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<String, String>> {
        let resolver = self.resolver.clone();
        let path = path.to_string();
        Box::pin(async move {
            resolver
                .resolve_ipns_path(&path)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn eval_actor<'a>(
        &'a self,
        command: &'a str,
    ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move {
            let effective = normalize_actor_str(command);
            let (target, verb, str_args) = {
                let cfg = self.config.borrow();
                parse_actor_command(&effective, &**cfg).map_err(SchemeErr::MaError)?
            };
            let scheme_args: Vec<SchemeVal> = str_args.into_iter().map(SchemeVal::Str).collect();
            self.do_actor_call(&target, &verb, &scheme_args).await
        })
    }

    fn eval_actor_with_vals<'a>(
        &'a self,
        actor: &'a str,
        args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move {
            let effective = normalize_actor_str(actor);
            let (target, verb, _) = {
                let cfg = self.config.borrow();
                parse_actor_command(&effective, &**cfg).map_err(SchemeErr::MaError)?
            };
            self.do_actor_call(&target, &verb, args).await
        })
    }

    fn send_actor<'a>(
        &'a self,
        target: &'a str,
        verb: &'a str,
        args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        if let Err(error) = Did::validate_url(target) {
            return Box::pin(futures::future::ready(Err(error.to_string())));
        }
        let (sender, signing_key) = match self.build_signing_key("inbox") {
            Ok(pair) => pair,
            Err(e) => return Box::pin(futures::future::ready(Err(e))),
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
            &sender,
            target,
            ma_core::MESSAGE_TYPE_MESSAGE,
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
            let mut outbox = {
                let endpoint = self.endpoint.lock().await;
                endpoint
                    .outbox(resolver.as_ref(), &target_owned, INBOX_PROTOCOL_ID)
                    .await
                    .map_err(|e| e.to_string())?
            };
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
        if let Err(error) = Did::validate_url(target) {
            return Box::pin(futures::future::ready(Err(error.to_string())));
        }
        let (sender, signing_key) = match self.build_signing_key("inbox") {
            Ok(pair) => pair,
            Err(e) => return Box::pin(futures::future::ready(Err(e))),
        };
        let msg = match ma_core::Message::new(
            &sender,
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
            let mut outbox = {
                let endpoint = self.endpoint.lock().await;
                endpoint
                    .outbox(resolver.as_ref(), &target_owned, INBOX_PROTOCOL_ID)
                    .await
                    .map_err(|e| e.to_string())?
            };
            outbox.send(&msg).await.map_err(|e| e.to_string())?;
            Ok(msg_id)
        })
    }
}

// ── Private helpers ────────────────────────────────────────────────────────

/// Try fetching `path` from the local Kubo daemon; returns the response on
/// success so the caller can deserialise it as text or bytes.
async fn try_kubo_cat(
    http: &reqwest::Client,
    kubo_rpc_url: &str,
    path: &str,
) -> Option<reqwest::Response> {
    let url = format!(
        "{}/api/v0/cat?arg={}",
        kubo_rpc_url.trim_end_matches('/'),
        path
    );
    match http.post(&url).send().await {
        Ok(resp) if resp.status().is_success() => Some(resp),
        _ => None,
    }
}

/// Ensure the actor string has a leading `@` or `did:` prefix.
fn normalize_actor_str(input: &str) -> String {
    if input.starts_with('@') || input.starts_with("did:") {
        input.to_string()
    } else {
        format!("@{input}")
    }
}

// ── CBOR ↔ SchemeVal conversion ──────────────────────────────────────────

fn scheme_val_to_cbor(v: &SchemeVal) -> ciborium::Value {
    use ciborium::Value as V;
    match v {
        SchemeVal::Str(s) => V::Text(s.clone()),
        SchemeVal::Bytes(bytes) => V::Bytes(bytes.clone()),
        SchemeVal::Int(n) => V::Integer(ciborium::value::Integer::from(*n)),
        SchemeVal::Float(f) => V::Float(*f),
        SchemeVal::Bool(b) => V::Bool(*b),
        SchemeVal::Nil => V::Null,
        SchemeVal::List(items) => V::Array(items.iter().map(scheme_val_to_cbor).collect()),
        SchemeVal::Map(map) => V::Map(
            map.iter()
                .map(|(key, value)| (V::Text(key.clone()), scheme_val_to_cbor(value)))
                .collect(),
        ),
        // Lambdas and builtins can't be serialised — use their display string.
        other => V::Text(other.display()),
    }
}

fn cbor_to_scheme_val(v: &ciborium::Value) -> SchemeVal {
    use ciborium::Value as V;
    match v {
        V::Text(s) => SchemeVal::Str(s.clone()),
        V::Bytes(bytes) => SchemeVal::Bytes(bytes.clone()),
        V::Integer(n) => {
            let value = i128::from(*n);
            i64::try_from(value).map_or_else(|_| SchemeVal::Str(value.to_string()), SchemeVal::Int)
        }
        V::Float(f) => SchemeVal::Float(*f),
        V::Bool(b) => SchemeVal::Bool(*b),
        V::Null => SchemeVal::Nil,
        V::Array(items) => SchemeVal::List(items.iter().map(cbor_to_scheme_val).collect()),
        V::Map(pairs) => {
            let mut map = std::collections::BTreeMap::new();
            for (key, value) in pairs {
                let V::Text(key) = key else {
                    return SchemeVal::List(
                        pairs
                            .iter()
                            .map(|(key, value)| {
                                SchemeVal::List(vec![
                                    cbor_to_scheme_val(key),
                                    cbor_to_scheme_val(value),
                                ])
                            })
                            .collect(),
                    );
                };
                map.insert(key.clone(), cbor_to_scheme_val(value));
            }
            SchemeVal::Map(map)
        }
        V::Tag(_, inner) => cbor_to_scheme_val(inner),
        _ => SchemeVal::Str(format!("{v:?}")),
    }
}

fn decode_reply(payload: &[u8]) -> Result<SchemeVal, String> {
    use ciborium::Value as V;
    let val: V = match ciborium::de::from_reader(payload) {
        Ok(v) => v,
        Err(_) => return Ok(SchemeVal::Str(String::from_utf8_lossy(payload).to_string())),
    };
    match &val {
        // A bare :ok ack is an atom, not nothing — preserve it so callers
        // can see the success ack (display "ok", equality with ":ok") instead
        // of conflating it with Nil/().
        V::Text(s) if s == ":ok" => Ok(SchemeVal::Str(s.clone())),
        V::Array(items) => match (items.first(), items.get(1)) {
            (Some(V::Text(verb)), _) if verb == ":ok" => match items.get(1) {
                Some(v) => Ok(cbor_to_scheme_val(v)),
                // [:ok] with no payload is the array form of the bare ack.
                None => Ok(SchemeVal::Str(":ok".to_string())),
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

#[cfg(test)]
mod tests {
    use super::{cbor_to_scheme_val, decode_reply, scheme_val_to_cbor};
    use ciborium::Value as V;
    use ma_zscheme::SchemeVal;

    #[test]
    fn bytes_round_trip_through_cbor_value() {
        let value = SchemeVal::Bytes(vec![0x89, b'P', b'N', b'G']);
        let SchemeVal::Bytes(bytes) = cbor_to_scheme_val(&scheme_val_to_cbor(&value)) else {
            panic!("expected byte value");
        };
        assert_eq!(bytes, vec![0x89, b'P', b'N', b'G']);
    }

    fn cbor_bytes(value: &V) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn bare_ok_ack_is_preserved_as_an_atom_not_nil() {
        assert!(matches!(
            decode_reply(&cbor_bytes(&V::Text(":ok".to_string()))).unwrap(),
            SchemeVal::Str(s) if s == ":ok"
        ));
    }

    #[test]
    fn empty_ok_array_is_the_array_form_of_the_bare_ack() {
        assert!(matches!(
            decode_reply(&cbor_bytes(&V::Array(vec![V::Text(":ok".to_string())]))).unwrap(),
            SchemeVal::Str(s) if s == ":ok"
        ));
    }

    #[test]
    fn ok_array_with_payload_returns_the_payload() {
        assert!(matches!(
            decode_reply(&cbor_bytes(&V::Array(vec![
                V::Text(":ok".to_string()),
                V::Text("prop updated".to_string()),
            ])))
            .unwrap(),
            SchemeVal::Str(s) if s == "prop updated"
        ));
    }

    #[test]
    fn error_array_returns_the_reason() {
        assert_eq!(
            decode_reply(&cbor_bytes(&V::Array(vec![
                V::Text(":error".to_string()),
                V::Text("not authorised to edit props".to_string()),
            ])))
            .unwrap_err(),
            "not authorised to edit props"
        );
    }
}
