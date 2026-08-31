/// Scheme module — thin re-export of ma-zscheme, plus REPL line routing.
///
/// All evaluator logic lives in the `ma-zscheme` crate.
/// This module provides path-compatible re-exports so the rest of zscheme
/// can continue using `crate::scheme::*`.
pub use ma_zscheme::{eval_source, init_session_env, Ctx, SchemeErr, SchemeVal};

/// True when a submitted line is a whole-line dot command (`.my.path…` or
/// `#.my.path…`) that must be routed to the host dot grammar instead of the
/// Scheme reader. Bare dot paths are valid at the terminal, but never inside
/// Scheme parentheses.
#[must_use]
pub fn is_dot_command_line(source: &str) -> bool {
    let s = source.trim();
    if s.contains('\n') {
        return false;
    }
    let path = s.strip_prefix('#').unwrap_or(s);
    let mut chars = path.chars();
    chars.next() == Some('.') && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Evaluate a whole-line dot command via the host's dot grammar.
///
/// # Errors
///
/// Returns any error from the host's `eval_dot` handler.
pub fn eval_dot_line(source: &str, ctx: &Ctx) -> Result<SchemeVal, SchemeErr> {
    let s = source.trim();
    ctx.eval_dot(s.strip_prefix('#').unwrap_or(s))
}

/// Evaluate one submitted line: whole-line dot commands go straight to the
/// host dot grammar; everything else is evaluated as Scheme source.
///
/// # Errors
///
/// Returns any parse or evaluation error from the underlying handler.
pub async fn eval_line(source: &str, ctx: Ctx) -> Result<SchemeVal, SchemeErr> {
    if is_dot_command_line(source) {
        return eval_dot_line(source, &ctx);
    }
    eval_source(source, ctx).await
}

#[cfg(test)]
mod tests {
    use super::is_dot_command_line;
    use futures::{channel::oneshot, future::LocalBoxFuture};
    use ma_zscheme::{
        eval_source_in,
        parser::{parse_expr, tokenize},
        Ctx, Env, SchemeCtx, SchemeErr, SchemeVal,
    };
    use std::{path::Path, rc::Rc};

    struct StartupTestCtx;

    fn read_repo_file(path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
    }

    fn startup_scheme_source() -> String {
        for path in ["lib/.z.scheme", "lib/z.scheme"] {
            if let Ok(source) = read_repo_file(path) {
                return source;
            }
        }

        let mut source = String::new();
        for path in [
            "lib/stdlib.zscheme",
            "lib/runtime.zscheme",
            "lib/avatar.zscheme",
            "lib/events.zscheme",
        ] {
            source.push_str(
                &read_repo_file(path)
                    .unwrap_or_else(|error| panic!("read startup layer {path}: {error}")),
            );
            source.push('\n');
        }
        source
    }

    impl SchemeCtx for StartupTestCtx {
        fn eval_dot(&self, command: &str) -> Result<SchemeVal, SchemeErr> {
            if command == ".my.ctx.room" {
                Ok(SchemeVal::Str("did:ma:startup-test#room".to_string()))
            } else {
                Ok(SchemeVal::Nil)
            }
        }

        fn display_output(&self, _text: &str) {}

        fn resolve_target(&self, raw: &str) -> Result<String, String> {
            Ok(raw.to_string())
        }

        fn register_reply_sender(
            &self,
            _msg_id: String,
            _sender: oneshot::Sender<Result<SchemeVal, String>>,
        ) {
        }

        fn fetch_path<'a>(&'a self, _path: &'a str) -> LocalBoxFuture<'a, Result<String, String>> {
            Box::pin(async { Err("unavailable in startup test".to_string()) })
        }

        fn eval_actor<'a>(
            &'a self,
            _command: &'a str,
        ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
            Box::pin(async { Ok(SchemeVal::Nil) })
        }

        fn eval_actor_with_vals<'a>(
            &'a self,
            _actor: &'a str,
            _args: &'a [SchemeVal],
        ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
            Box::pin(async { Ok(SchemeVal::Map(std::collections::BTreeMap::default())) })
        }

        fn send_rpc<'a>(
            &'a self,
            _target: &'a str,
            _verb: &'a str,
            _args: &'a [SchemeVal],
        ) -> LocalBoxFuture<'a, Result<String, String>> {
            Box::pin(async { Ok("startup-test".to_string()) })
        }

        fn send_text<'a>(
            &'a self,
            _target: &'a str,
            _body: &'a str,
        ) -> LocalBoxFuture<'a, Result<String, String>> {
            Box::pin(async { Ok("startup-test".to_string()) })
        }
    }

    #[test]
    fn detects_bare_and_hash_dot_lines() {
        assert!(is_dot_command_line(".my.aliases.sky: did:ma:bar"));
        assert!(is_dot_command_line("#.my.aliases.sky: bar"));
        assert!(is_dot_command_line(".my.aliases.sky"));
        assert!(is_dot_command_line("  .my.inbox!flush  "));
    }

    #[test]
    fn rejects_scheme_source() {
        assert!(!is_dot_command_line("(#.my.aliases.sky: \"bar\")"));
        assert!(!is_dot_command_line("(+ 1 2)"));
        assert!(!is_dot_command_line("#t"));
        assert!(!is_dot_command_line(".5"));
        assert!(!is_dot_command_line("..broken"));
        assert!(!is_dot_command_line("(define x 1)\n.my.x"));
    }

    #[test]
    fn generated_startup_scheme_parses() {
        let source = startup_scheme_source();
        let tokens = tokenize(&source).expect("tokenise generated startup Scheme");
        let mut position = 0;
        while position < tokens.len() {
            let (_, next_position) =
                parse_expr(&tokens, position).expect("parse generated startup Scheme");
            position = next_position;
        }
    }

    #[test]
    fn generated_startup_scheme_loads_and_resolves_default_pool() {
        let mut source = startup_scheme_source();
        source.push_str("\n(default-pool)\n");
        let context: Ctx = Rc::new(StartupTestCtx);
        futures::executor::block_on(eval_source_in(&source, Env::new_root(), context))
            .expect("load generated startup Scheme and resolve default pool");
    }
}
