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
}
