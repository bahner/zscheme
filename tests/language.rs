use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::{channel::oneshot, future::LocalBoxFuture};
use ma_zscheme::{
    eval_source, init_session_env, parse_dot_command, Ctx, DotOp, DotRegistry, InMemoryRegistry,
    SchemeCtx, SchemeErr, SchemeVal,
};

#[derive(Default)]
struct TestCtx {
    config: RefCell<InMemoryRegistry>,
    output: RefCell<String>,
}

impl SchemeCtx for TestCtx {
    fn eval_dot(&self, command: &str) -> Result<SchemeVal, SchemeErr> {
        let (path, op) = parse_dot_command(command)
            .ok_or_else(|| SchemeErr::MaError(format!("bad dot command: {command}")))?;

        match op {
            DotOp::Get => {
                if let Some(value) = self.config.borrow().get(&path) {
                    Ok(SchemeVal::Str(value))
                } else {
                    let pairs = self.config.borrow().list(&path);
                    if pairs.is_empty() {
                        Err(SchemeErr::MaError(format!(
                            "no value at .{}",
                            path.replace('/', ".")
                        )))
                    } else {
                        Ok(SchemeVal::List(
                            pairs
                                .into_iter()
                                .map(|(key, _)| SchemeVal::Str(key))
                                .collect(),
                        ))
                    }
                }
            }
            DotOp::Set(value) => {
                self.config.borrow_mut().set(&path, &value);
                Ok(SchemeVal::Nil)
            }
            DotOp::Delete => {
                self.config.borrow_mut().delete_subtree(&path);
                Ok(SchemeVal::Nil)
            }
            DotOp::Meta { verb, args } => Err(SchemeErr::MaError(format!(
                "unsupported test dot meta: .{}!{} {}",
                path.replace('/', "."),
                verb,
                args
            ))),
        }
    }

    fn display_output(&self, text: &str) {
        self.output.borrow_mut().push_str(text);
    }

    fn resolve_target(&self, raw: &str) -> Result<String, String> {
        self.config.borrow().resolve_target(raw)
    }

    fn register_reply_sender(
        &self,
        _msg_id: String,
        _sender: oneshot::Sender<Result<SchemeVal, String>>,
    ) {
    }

    fn fetch_path<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Err(format!("no remote fetch in tests: {path}")) })
    }

    fn eval_actor<'a>(&'a self, cmd: &'a str) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move { Err(SchemeErr::MaError(format!("no actor RPC in tests: {cmd}"))) })
    }

    fn eval_actor_with_vals<'a>(
        &'a self,
        actor: &'a str,
        _args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<SchemeVal, SchemeErr>> {
        Box::pin(async move {
            Err(SchemeErr::MaError(format!(
                "no actor RPC in tests: {actor}"
            )))
        })
    }

    fn send_rpc<'a>(
        &'a self,
        target: &'a str,
        verb: &'a str,
        _args: &'a [SchemeVal],
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Err(format!("no RPC in tests: {target} {verb}")) })
    }

    fn send_text<'a>(
        &'a self,
        target: &'a str,
        _body: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Err(format!("no inbox send in tests: {target}")) })
    }
}

fn eval(source: &str) -> Result<(SchemeVal, Rc<TestCtx>), SchemeErr> {
    init_session_env();
    let test_ctx = Rc::new(TestCtx::default());
    let ctx: Ctx = test_ctx.clone();
    let value = futures::executor::block_on(eval_source(source, ctx))?;
    Ok((value, test_ctx))
}

fn eval_file(path: &Path) -> Result<SchemeVal, SchemeErr> {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read functional test {}: {err}", path.display()));
    eval(&source).map(|(value, _)| value)
}

#[test]
fn unit_dot_parser_accepts_zion_dot_notation() {
    let (path, op) = parse_dot_command(".my.aliases.sky").unwrap();
    assert_eq!(path, "my/aliases/sky");
    assert!(matches!(op, DotOp::Get));

    let (path, op) = parse_dot_command(".my.i18n: nb").unwrap();
    assert_eq!(path, "my/i18n");
    assert!(matches!(op, DotOp::Set(value) if value == "nb"));
}

#[test]
fn unit_dot_parser_rejects_legacy_slash_local_config() {
    assert!(parse_dot_command("/my/aliases/sky").is_none());
    assert!(parse_dot_command("my/aliases/sky").is_none());
}

#[test]
fn unit_evaluator_reads_writes_and_deletes_dot_config() {
    let source = r#"
        (.my.i18n: "nb")
        (assert (equal? (.my.i18n) "nb"))
        (.my.i18n:)
        (guard (e (#t "deleted")) (.my.i18n))
    "#;

    let (value, _) = eval(source).unwrap();
    assert_eq!(value.display(), "deleted");
}

#[test]
fn unit_evaluator_rejects_hash_slash_my_config() {
    let error = match eval("(#/my/i18n)") {
        Ok((value, _)) => panic!("expected #/my config to fail, got {}", value.display()),
        Err(error) => error,
    };
    assert!(!error.to_string().is_empty());
}

#[test]
fn unit_include_loads_from_dot_config_path() {
    let source = r#"
        (.my.doc.lib: "(define (triple x) (* x 3))")
        (include ".my.doc.lib")
        (triple 14)
    "#;

    let (value, _) = eval(source).unwrap();
    assert_eq!(value.display(), "42");
}

#[test]
fn unit_display_and_newline_route_to_host_output() {
    let source = r#"
        (display "hello")
        (newline)
        (write "world")
    "#;

    let (_, test_ctx) = eval(source).unwrap();
    assert_eq!(test_ctx.output.borrow().as_str(), "hello\n\"world\"");
}

#[test]
fn unit_dot_subtree_listing_returns_dot_paths() {
    let source = r#"
        (.my.aliases.sky: "did:ma:sky")
        (.my.aliases.ms: "did:ma:ms")
        (.my.aliases)
    "#;

    let (value, _) = eval(source).unwrap();
    assert_eq!(value.display(), "(\".my.aliases.ms\" \".my.aliases.sky\")");
}

#[test]
fn unit_dot_alias_storage_feeds_target_resolution() {
    let source = r#"
        (.my.aliases.sky: "did:ma:sky")
    "#;

    let (_, test_ctx) = eval(source).unwrap();
    assert_eq!(
        test_ctx.config.borrow().resolve_target("@sky#room"),
        Ok("did:ma:sky#room".to_string())
    );
}

#[test]
fn functional_scheme_programs_pass() {
    let mut paths: Vec<PathBuf> = fs::read_dir("tests")
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("z"))
        .collect();

    let functional_dir = Path::new("tests/functional");
    if functional_dir.exists() {
        paths.extend(
            fs::read_dir(functional_dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("z")),
        );
    }

    paths.sort();
    assert!(
        !paths.is_empty(),
        "expected at least one functional .z test"
    );

    for path in paths {
        eval_file(&path).unwrap_or_else(|err| panic!("{} failed: {err}", path.display()));
    }
}
