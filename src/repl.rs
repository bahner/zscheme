/// Interactive REPL for zscheme — readline-backed with persistent history.
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::context::Ctx;
use crate::scheme::{eval_source, SchemeVal};

const PROMPT: &str = "zscheme> ";
const PROMPT_CONT: &str = "    ... ";

/// Evaluation backend for the REPL loop.
///
/// `Ok(None)` means the result was nil (print nothing); `Ok(Some(s))` is a
/// display string for stdout; `Err(s)` is a pre-formatted error for stderr.
#[allow(async_fn_in_trait)] // single-threaded LocalSet usage — no Send bound wanted
pub trait ReplEval {
    async fn eval(&mut self, source: &str) -> Result<Option<String>, String>;
}

/// In-process evaluator (standalone mode).
pub struct LocalEval(pub Ctx);

impl ReplEval for LocalEval {
    async fn eval(&mut self, source: &str) -> Result<Option<String>, String> {
        match eval_source(source, self.0.clone()).await {
            Ok(SchemeVal::Nil) => Ok(None),
            Ok(val) => Ok(Some(val.display())),
            Err(e) => Err(crate::daemon::format_scheme_err(&e)),
        }
    }
}

/// Run an interactive read-eval-print loop with readline editing and history.
///
/// History is persisted to `$XDG_DATA_HOME/ma/zscheme_history`.
/// Multi-line expressions are buffered until parentheses balance.
/// Ctrl-C clears the current buffer; Ctrl-D / EOF exits.
pub async fn run_repl<E: ReplEval>(mut evaluator: E) -> anyhow::Result<()> {
    let history_path = history_file_path();

    let mut rl = DefaultEditor::new().map_err(|e| anyhow::anyhow!("readline init: {e}"))?;
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    eprintln!("zscheme  —  Ctrl-D or (exit) to quit\n");

    let mut buffer = String::new();
    let mut depth: i32 = 0;

    loop {
        let prompt = if buffer.is_empty() {
            PROMPT
        } else {
            PROMPT_CONT
        };

        let line = match rl.readline(prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: clear current buffer, start fresh
                if !buffer.is_empty() {
                    buffer.clear();
                    depth = 0;
                    eprintln!("^C");
                } else {
                    eprintln!("^C  (Ctrl-D or (exit) to quit)");
                }
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        };

        // Add to history only when starting a new expression
        if buffer.is_empty() && !line.trim().is_empty() {
            let _ = rl.add_history_entry(&line);
        }

        // Handle single-line quit commands
        if buffer.is_empty() {
            match line.trim() {
                ":quit" | ":q" | ":exit" | "(exit)" => break,
                _ => {}
            }
        }

        // Accumulate and track paren depth
        for ch in line.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
        buffer.push_str(&line);
        buffer.push('\n');

        // Complete expression when parens are balanced (or no parens at all)
        if depth <= 0 {
            depth = 0;
            let source = buffer.trim().to_string();
            buffer.clear();

            if source.is_empty() {
                continue;
            }

            match evaluator.eval(&source).await {
                Ok(None) => {}
                Ok(Some(text)) => println!("{text}"),
                Err(msg) => eprintln!("{msg}"),
            }
        }
    }

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }

    Ok(())
}

fn history_file_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|b| {
        let dir = b.data_dir().join("ma");
        std::fs::create_dir_all(&dir).ok();
        dir.join("zscheme_history")
    })
}
