# zscheme - Agent Notes

## Agent rules

- Never modify files outside this workspace without explicit user approval.
- Use British English for project-owned names and prose.
- Keep `ma-core` and `ma-zscheme` as published dependencies; do not commit local path dependencies.

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
