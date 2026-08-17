# zscheme - Agent Notes

## Agent rules

- Never modify files outside this workspace without explicit user approval.
- Use British English for project-owned names and prose.
- Write DRY, KISS code: avoid duplicated logic and prefer the simplest
  implementation that meets the requirement.
- Keep `ma-core` `^0.14.4` or newer and `ma-zscheme` as published dependencies; do not commit local path dependencies.

## DID document publication

- Canonicalise a loaded bundle's legacy `created_at` to RFC 3339 UTC
	whole-second form and persist the migrated bundle before building documents.
- Use `SecretBundle::build_document` so `updatedAt` is renewed and the final
	extension data is covered by the proof.
- Call `Document::validate()` and `Document::verify()` immediately before every
	direct `IpfsDidPublisher::publish_document` call.

## Stdlib vs runtime ownership

The Scheme stdlib is deliberately generic. It contains purely evaluative helpers
common to all host applications, such as list utilities, string predicates, and
plain data transforms. MA actor, DID, ctx, and object-reference helpers belong
in `lib/runtime.zscheme`, not in stdlib. In particular, `resolve-ref` belongs to
the runtime lookup vocabulary.

`unique-list` is the generic shared list-set primitive that turns a candidate
list into a stable, de-duplicated answer. The runtime-specific `resolve-ref`
then adopts that shape as part of its answer contract: a flat list of DID
strings, with repeated DIDs collapsed before it crosses the avatar boundary.

## Events and avatar layer

Events do not belong to the runtime as an authoritative semantic surface. They
are a client-side, Zion-facing convenience stream: a human terminal can consume
room broadcasts / `:print`-style traffic as a narrative overlay, while the
runtime answers ordinary RPC/data questions through `ma-reply!` and structured
ctx maps. In other words, the event channel is consumer-visible machinery for
Zion, not a second world protocol to be reified in the runtime library.

The avatar layer is a client-side facade over the data forms. It resolves a
human word against the room snapshot and inventory, chooses a single `did:`
target when that is appropriate, and then calls the runtime actor methods such
as `:set-parent` / `:claim` / `:owner?` that implement the object movement and
ownership lifecycle. The avatar therefore owns the human-facing interpretation
and the one-argument convenience wrappers, not the authoritative runtime
semantics themselves.

All lambda-ma avatar and play-time policy belongs exclusively in the composed
`.z.scheme` layers, particularly `lib/avatar.zscheme` and `lib/events.zscheme`.
The host delivers typed unsolicited events but must not sequence `:hold`,
`:child`, `:set-parent`, `:claim`, `:drop`, `:put`, or any other world verb.
`on-event` in zscheme owns `:parent` handshakes and every
container/duckie/object-transfer decision.

All object arguments use one room-child resolver over `who`, `agents`,
`things`, and `exits`, followed by inventory contents. Exactly one match is
accepted; no match is an error, and multiple matches list their DID/DID-URLs so
the user can rephrase or supply an exact address. An ambiguous candidate also
shows `in inventory` when its ctx parent matches `.my.ctx.inv`, or `in <room
name>` when its parent matches the cached room ctx actor. Other parents are not
looked up or labelled. A bare DID has no identity actor, so `look` renders the
resolved child ctx rather than requiring an RPC.
Ordinary object RPC verbs dispatch through `(command object method . params)`;
do not duplicate `resolve-one` plus `actor-call` in each command wrapper.
`look <object>` uses the same room-plus-inventory candidate pool, but renders
the resolved child ctx locally rather than calling the target actor.

`give <object...> to <person...>` is a consent-based ownership offer. The
object resolves through the ordinary room-plus-inventory pool to a DID-URL;
the recipient resolves only through the room's `who` entries to a different
bare DID. The avatar sets a host-generated one-time recovery secret on the
object, then sends the recipient an ordinary text message containing the full
`claim <object-did-url> <secret>` command. It never changes `owner` directly,
and received offer text is never evaluated automatically.

`lib/stdlib.zscheme`, `lib/runtime.zscheme`, `lib/avatar.zscheme`, and
`lib/events.zscheme` are the four authoritative development layers.
`lib/.z.scheme` is their ignored, generated physical concatenation in that
order; `lib/.z.scheme.cid` is the versionable publication result. Use
`make zscheme-cid` to publish only that combined startup source. Keep the
composition ordinary Scheme; do not add a namespace, module, or dynamic loading
framework around it.

## Scheme host contract

The evaluator is provided by `ma-zscheme`; this repository supplies native I/O through `CliCtx`.

Inside Scheme parentheses, programs use hash-dot local paths exclusively: `#.my.path`. Never accept bare dot paths as Scheme list heads. This does not change any host terminal command grammar outside Scheme expressions. The evaluator strips `#` only when calling the internal `SchemeCtx::eval_dot` host method.

- `ipfs-get` uses `SchemeCtx::fetch_bytes` and returns opaque bytes.
- `ipfs-cat` uses `SchemeCtx::fetch_path` and returns UTF-8 text.
- `ipfs-name-resolve` uses `SchemeCtx::resolve_ipns` and returns an `/ipfs/<cid>` reference without loading content.
- `include` is the only one of these operations that evaluates fetched Scheme source.
- Preserve `SchemeVal::Bytes` as CBOR byte strings and string-keyed maps as CBOR maps in RPC traffic.
- Delegate IPNS gateway selection and resolution metadata parsing to the shared `ma_core::IpfsGatewayResolver`; do not duplicate it in `CliCtx`.

For unreleased cross-repository APIs, validate with temporary Cargo `--config patch.crates-io.<crate>.path=...` overrides and leave `Cargo.toml` and `Cargo.lock` on registry sources.
