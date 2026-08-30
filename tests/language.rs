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

#[test]
fn production_events_render_humans_and_agents_as_occupants() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (event-look
                    (make-map "name" "Atrium" "description" "Quiet."
                              "children"
                              (list (make-map "actor" "did:ma:alice" "kind" "agent"
                                              "name" "Alice")
                                    (make-map "actor" "did:ma:world#attila" "kind" "agent"
                                              "name" "Attila")
                                    (make-map "actor" "did:ma:world#mirror" "kind" "exit"
                                              "direction" "mirror"))))
                "#
    );

    let (_, ctx) = eval(&source).unwrap();
    assert_eq!(
        ctx.output.borrow().as_str(),
        "Atrium\nQuiet.\nOccupants:\nAlice\nAttila\nThe room appears to be empty.\nExits:\nmirror"
    );
}

#[test]
fn production_room_events_mutate_cached_children_and_snapshots_replace_them() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (define alice
                    (make-map "actor" "did:ma:alice" "kind" "agent" "name" "Alice"))
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing" "name" "Lamp"))
                (remember-room!
                    (make-map "actor" "did:ma:world#room" "name" "Room"
                              "children" (make-map "did:ma:alice" alice)))

                (on-event ":arrive" (list lamp))
                (on-event ":arrive" (list (map-set lamp "nick" "Brass Lamp")))
                (assert (= (length (room-child-pool last-room)) 2))
                (assert (equal?
                    (map-ref (find-entry-by-actor (room-child-pool last-room)
                                                  "did:ma:world#lamp")
                             "nick" "")
                    "Brass Lamp"))

                (on-event ":leave" (list lamp))
                (on-event ":leave" (list lamp))
                (assert (equal? (map entry-actor (room-child-pool last-room))
                                (list "did:ma:alice")))

                (remember-room!
                    (make-map "actor" "did:ma:world#room" "name" "Fresh"
                              "children" (make-map "did:ma:world#lamp" lamp)))
                (assert (equal? (map entry-actor (room-child-pool last-room))
                                (list "did:ma:world#lamp")))
                "room-event-cache-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "room-event-cache-ok");
}

#[test]
fn production_speech_events_take_a_context_and_text() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (define duckie
                    (make-map "actor" "did:ma:world#duckie" "name" "Duckie"))
                (on-event ":say" (list duckie "quack"))
                (on-event ":emote" (list duckie "dances"))
                "speech-events-ok"
        "#
    );

    let (value, ctx) = eval(&source).unwrap();
    assert_eq!(value.display(), "speech-events-ok");
    assert_eq!(ctx.output.borrow().as_str(), "Duckie: quackDuckie dances");
}

fn eval_file(path: &Path) -> Result<SchemeVal, SchemeErr> {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read functional test {}: {err}", path.display()));
    eval(&source).map(|(value, _)| value)
}

#[test]
fn unit_host_dot_parser_accepts_normalised_paths() {
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
        (#.my.i18n: "nb")
        (assert (equal? (#.my.i18n) "nb"))
        (#.my.i18n:)
        (guard (e (#t "deleted")) (#.my.i18n))
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
fn unit_evaluator_rejects_bare_dot_my_config() {
    let error = match eval("(.my.i18n)") {
        Ok((value, _)) => panic!("expected bare .my config to fail, got {}", value.display()),
        Err(error) => error,
    };
    assert!(!error.to_string().is_empty());
}

#[test]
fn unit_include_loads_from_dot_config_path() {
    let source = r#"
        (#.my.doc.lib: "(define (triple x) (* x 3))")
        (include #.my.doc.lib)
        (triple 14)
    "#;

    let (value, _) = eval(source).unwrap();
    assert_eq!(value.display(), "42");
}

#[test]
fn unit_stdlib_provides_list_accessors() {
    let stdlib = fs::read_to_string("lib/stdlib.zscheme").unwrap();
    let source = format!(
        r#"
        {stdlib}

        (assert (= (caar '((1 2) 3 4)) 1))
        (assert (equal? (cdar '((1 2) 3 4)) '(2)))
        (assert (= (cadr '(1 2 3 4)) 2))
        (assert (equal? (cddr '(1 2 3 4)) '(3 4)))
        (assert (= (caadr '(0 (1 2) 3)) 1))
        (assert (equal? (cdadr '(0 (1 2) 3)) '(2)))
        (assert (= (cadddr '(0 1 2 3 4)) 3))
        (assert (equal? (cddddr '(0 1 2 3 4 5)) '(4 5)))
        (assert (= (fib 10) 55))
        "list-accessors-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "list-accessors-ok");
}

#[test]
fn production_runtime_resolves_ctx_references() {
    let stdlib = fs::read_to_string("lib/stdlib.zscheme").unwrap();
    let runtime = fs::read_to_string("lib/runtime.zscheme").unwrap();
    let source = format!(
        r#"
        {stdlib}
        {runtime}

        (define pool
          (list
            (make-map "actor" "did:ma:lamp" "name" "Brass Lamp"
                      "nick" "Light" "description" "A golden light")
            (make-map "did" "did:ma:desk" "name" "Writing Desk"
                      "nick" "Table" "description" "A wooden desk")
            (make-map "actor" "did:ma:lamp" "name" "Spare Lamp")))

        (assert (equal? (resolve-ref "brass" pool) '("did:ma:lamp")))
        (assert (equal? (resolve-ref "LIGHT" pool) '("did:ma:lamp")))
        (assert (equal? (resolve-ref "wooden" pool) '("did:ma:desk")))
        (assert (equal? (resolve-ref "desk" pool) '("did:ma:desk")))
        (assert (equal? (resolve-ref "lamp" pool) '("did:ma:lamp")))
        (assert (equal? (resolve-ref "missing" pool) '()))
        (assert (equal?
          (resolve-ref "shared"
            (list (make-map "actor" "did:ma:one" "name" "Shared one")
                  (make-map "actor" "did:ma:two" "name" "Shared two")))
          '("did:ma:one" "did:ma:two")))
        "runtime-resolver-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "runtime-resolver-ok");
}

#[test]
fn production_avatar_resolves_exactly_one_exit_actor() {
    let source = ["stdlib", "runtime", "avatar"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (set! last-room
                    (make-map "children"
                        (list (make-map "actor" "did:ma:exit-hull" "kind" "exit"
                                        "direction" "hull"))))
                (assert (equal? (resolve-exit "hull") "did:ma:exit-hull"))

                (set! last-room
                    (make-map "children"
                        (list (make-map "actor" "did:ma:exit-one" "kind" "exit"
                                        "direction" "east hatch")
                              (make-map "actor" "did:ma:exit-two" "kind" "exit"
                                        "direction" "east door"))))
                (guard (error
                                ((string-contains error "matches more than one") "avatar-exit-resolver-ok")
                                (#t (error error)))
                    (resolve-exit "east"))
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-exit-resolver-ok");
}

#[test]
fn production_avatar_inventory_is_never_forged_by_a_view() {
    let source = ["stdlib", "runtime", "avatar"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.ctx.runtime: "did:ma:world")
                (define forge-target #f)
                (define (actor-call actor method . args)
                    (when (equal? method "forge")
                        (set! forge-target actor))
                    "did:ma:world#inventory")

                (assert (equal? (root) "did:ma:world#root"))
                ; a view never forges: with no inventory configured, none is
                ; created, and the pointer stays unset.
                (assert (equal? (my-inv-if-any) #f))
                (assert (equal? forge-target #f))

                (#.my.ctx.inv: "did:ma:elsewhere#travelling-bag")
                (set! forge-target #f)
                (assert (equal? (my-inv-if-any) "did:ma:elsewhere#travelling-bag"))
                (assert (equal? forge-target #f))
                "avatar-inventory-pure-view"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-inventory-pure-view");
}

const AVATAR_TEST_PREAMBLE: &str = r#"
                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.runtime: "did:ma:world")
                (#.my.ctx.room: "did:ma:world#room")
                (#.my.ctx.nick: "tester")
                (#.my.ctx.inv: "did:ma:world#inventory")

                (define lamp (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                                       "parent" "did:ma:world#room" "name" "Brass Lamp"))
                (define box (make-map "actor" "did:ma:world#box" "kind" "container"
                                      "parent" "did:ma:world#room" "name" "Wooden Box"))
                (define coin (make-map "actor" "did:ma:world#coin" "kind" "thing"
                                       "parent" "did:ma:world#inventory" "name" "Silver Coin"))
                (define hull-exit
                    (make-map "actor" "did:ma:world#exit-hull" "kind" "exit"
                              "parent" "did:ma:world#room" "direction" "hull"))
                (define room-snapshot
                    (make-map "actor" "did:ma:world#room" "parent" "did:ma:world#room"
                                        "nick" "tester" "name" "Test room" "description" "Ready."
                                        "children" (make-map "did:ma:world#lamp" lamp
                                                             "did:ma:world#exit-hull" hull-exit)))
                (set! last-room room-snapshot)

                (define actor-calls ())
                (define coin-confirmed #f)
                (define (actor-call actor method . args)
                    (assert (string? actor))
                    (assert (string? method))
                    (set! actor-calls (append actor-calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#coin")
                                (equal? method "child"))
                           (begin (set! coin-confirmed #t) ()))
                          ((and (equal? actor "did:ma:world#coin")
                                (equal? method "set-parent")
                                (not coin-confirmed))
                           (error "set-parent must be requested by current parent"))
                          ((equal? method "look") room-snapshot)
                                  ((and (equal? actor "did:ma:world#inventory")
                                      (equal? method "contents?")) (list coin box))
                                  ((and (equal? actor "did:ma:world#box")
                                      (equal? method "contents?")) (list coin))
                                  ((equal? method "contents?") ())
                                ((equal? method "traverse")
                                 (make-map "parent" "did:ma:world#room" "nick" "tester"))
                                ((equal? method "enter") room-snapshot)
                                ((equal? method "kind?") "/ma/container/0.0.1")
                                ((equal? method "owner?") "did:ma:owner")
                                ((equal? method "about") "Brass Lamp\nA test lamp.")
                                ((equal? method "who?") ())
                                ((equal? method "occupants?") ())
                                ((equal? method "things?") (list lamp box))
                                ((equal? method "exits?") (list hull-exit))
                                ((equal? method "help") "Actor help")
                                    ((and (equal? actor "did:ma:world#box")
                                        (equal? method "roll-call")
                                        (equal? (car args) "begin")) "roll-call begun")
                                ((equal? method "forge") "did:ma:world#new-thing")
                                ((equal? method "remove") "removed")
                                (else ())))

                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
"#;

const AVATAR_TEST_SMOKES: &str = r#"
                (smoke "go" (lambda () (go "hull")))
                (smoke "forge" (lambda () (forge "thing" "named" "Test Thing")))
                (smoke "enter" (lambda () (enter "did:ma:world#room" "tester")))
                (smoke "say" (lambda () (say "hello" "world")))
                (smoke "emote" (lambda () (emote "waves")))
                (smoke "claim" (lambda () (claim "lamp")))
                (smoke "tell" (lambda () (tell "lamp" "to" "ping" "once")))
                (smoke "equip" (lambda () (equip "did:ma:world#box")))
                (smoke "inv show" (lambda () (inv)))
                (smoke "look" (lambda () (look)))
                (smoke "look target" (lambda () (look "lamp")))
                "avatar-commands-ok"
"#;

fn avatar_command_test_source() -> String {
    let libs = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    format!("{libs}\n{AVATAR_TEST_PREAMBLE}\n{AVATAR_TEST_SMOKES}")
}

#[test]
fn production_avatar_commands_accept_representative_arguments() {
    let (value, _) = eval(&avatar_command_test_source()).unwrap();
    assert_eq!(value.display(), "avatar-commands-ok");
}

#[test]
fn production_avatar_parent_proposal_is_acknowledged_and_held() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.inv: "did:ma:world#inventory")
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    ())

                (on-event ":parent"
                           (list (make-map "actor" "did:ma:world#lamp"
                                           "parent" "did:ma:me")))
                (assert (equal? calls
                    (list (list "did:ma:world#lamp" "child"
                                (list (make-map "actor" "did:ma:world#lamp"
                                                "parent" "did:ma:me"))))))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#lamp"))
                "parent-handshake-ok"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "parent-handshake-ok");
}

#[test]
fn production_avatar_transfer_commands_match_lambda_ma_rpcs() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (#.my.ctx.inv: "did:ma:world#inventory")
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Lamp"))
                (define box
                    (make-map "actor" "did:ma:world#box" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Box"))
                (define coin
                    (make-map "actor" "did:ma:world#coin" "kind" "thing"
                              "parent" "did:ma:world#inventory" "name" "Coin"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#lamp" lamp
                                                   "did:ma:world#box" box)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#inventory")
                                (equal? method "kind?"))
                           "/ma/container/0.0.1")
                          ((and (equal? actor "did:ma:world#inventory")
                                (equal? method "contents?"))
                           (list coin))
                          ((and (equal? actor "did:ma:world#box")
                                (equal? method "contents?"))
                           (list coin))
                          (else ())))

                (set! calls ())
                (smoke "take" (lambda () (take "Lamp")))
                (assert (equal? calls
                    (list (list "did:ma:world#lamp" "hold" ()))))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me")))

                (set! calls ())
                (smoke "take from" (lambda () (take "Coin" "from" "Box")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#box" "take" (list "did:ma:world#coin"))))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me")))

                (set! calls ())
                (smoke "take-from" (lambda () (take-from "Box" "Coin")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#box" "take" (list "did:ma:world#coin"))))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me")))

                (set! calls ())
                (smoke "drop" (lambda () (drop)))
                    (let ((reversed (reverse calls)))
                    (assert (equal? (car reversed)
                        (list "did:ma:world#coin" "set-parent"
                            (list "did:ma:world#room"))))
                    (assert (equal? (cadr reversed)
                        (list "did:ma:world#room" "drop"
                            (list "did:ma:world#coin")))))

                (set! calls ())
                (smoke "take-from for put" (lambda () (take-from "Box" "Coin")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me")))
                (set! calls ())
                (smoke "put" (lambda () (put "Coin" "in" "Box")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#coin" "put" (list "did:ma:world#box"))))
                "transfer-rpcs-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "transfer-rpcs-ok");
}

#[test]
fn production_avatar_equip_books_inventory_and_drops_old() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (define box
                    (make-map "actor" "did:ma:world#box" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Box"))
                (define crate
                    (make-map "actor" "did:ma:world#crate" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Crate"))
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Lamp"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#box" box
                                                   "did:ma:world#crate" crate
                                                   "did:ma:world#lamp" lamp)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#lamp")
                                (equal? method "kind?")) "thing")
                          ((equal? method "kind?") "/ma/container/0.0.1")
                          ((equal? method "contents?") ())
                          (else ())))

                ; equip from the room: the same :hold take uses, but the container
                ; is booked into the inventory slot — only once :parent arrives.
                (set! calls ())
                (smoke "equip" (lambda () (equip "Box")))
                (assert (equal? calls
                    (list (list "did:ma:world#box" "kind?" ())
                          (list "did:ma:world#box" "hold" ()))))
                (assert (not (my-inv-if-any)))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#box"
                                                      "parent" "did:ma:me"
                                                      "name" "Box")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#box"))
                ; never lands in the held slot, and its ctx is cached so the
                ; inventory container itself resolves by name.
                (assert (null? (hand-pool)))
                (assert (equal? (entry-actor (car (resolve-inventory-pool)))
                                "did:ma:world#box"))

                ; a second equip drops the previous inventory container to the
                ; room, after the new one's :child ack.
                (set! calls ())
                (smoke "equip second" (lambda () (equip "Crate")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#crate"
                                                      "parent" "did:ma:me"
                                                      "name" "Crate")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#crate"))
                (let ((r (reverse calls)))
                    (assert (equal? (car r)
                        (list "did:ma:world#box" "set-parent"
                            (list "did:ma:world#room"))))
                    (assert (equal? (cadr r)
                        (list "did:ma:world#room" "drop"
                            (list "did:ma:world#box"))))
                    (assert (equal? (caddr r)
                        (list "did:ma:world#crate" "child"
                            (list (make-map "actor" "did:ma:world#crate"
                                            "parent" "did:ma:me"
                                            "name" "Crate"))))))
                (assert (null? (hand-pool)))

                ; equipping a non-container is refused before any :hold.
                (set! calls ())
                (guard (e ((string-contains e "invalid inventory kind") #t)
                          (#t (error (string-append "expected kind refusal, got: " e))))
                    (equip "Lamp")
                    (error "equip did not refuse"))
                (assert (equal? calls (list (list "did:ma:world#lamp" "kind?" ()))))

                ; inv takes no arguments; the bare form just renders.
                (smoke "inv" (lambda () (inv)))
                (guard (e ((string-contains e "usage: inv") #t)
                          (#t (error (string-append "expected usage error, got: " e))))
                    (inv "Box")
                    (error "inv did not refuse"))
                "equip-slot-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "equip-slot-ok");
}

#[test]
fn production_avatar_equip_from_container_and_nested_inv() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (define box
                    (make-map "actor" "did:ma:world#box" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Box"))
                (define chest
                    (make-map "actor" "did:ma:world#chest" "kind" "container"
                              "parent" "did:ma:world#box" "name" "Chest"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#box" box)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#box")
                                (equal? method "contents?")) (list chest))
                          ((equal? method "kind?") "/ma/container/0.0.1")
                          ((equal? method "contents?") ())
                          (else ())))

                ; box becomes the inventory from the room first, so the nested
                ; case below has box as the inv with chest inside it.
                (set! calls ())
                (smoke "equip box" (lambda () (equip "Box")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#box"
                                                      "parent" "did:ma:me"
                                                      "name" "Box")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#box"))

                ; the nested case: chest sits inside box, the current inv. Box
                ; resolves via the cached inv-entry, chest goes through box's
                ; :take gate, and the old box is dropped to the room only after
                ; chest's :child ack.
                (set! calls ())
                (smoke "equip nested" (lambda () (equip "Chest" "from" "Box")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#chest"
                                                      "parent" "did:ma:me"
                                                      "name" "Chest")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#chest"))
                (let ((r (reverse calls)))
                    (assert (equal? (car r)
                        (list "did:ma:world#box" "set-parent"
                            (list "did:ma:world#room"))))
                    (assert (equal? (cadr r)
                        (list "did:ma:world#room" "drop"
                            (list "did:ma:world#box"))))
                    (assert (equal? (caddr r)
                        (list "did:ma:world#chest" "child"
                            (list (make-map "actor" "did:ma:world#chest"
                                            "parent" "did:ma:me"
                                            "name" "Chest")))))
                    ; the pick-up is box's :take lock gate, never a :hold.
                    (assert (equal? (cadddr r)
                        (list "did:ma:world#box" "take"
                            (list "did:ma:world#chest")))))
                "equip-from-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "equip-from-ok");
}

#[test]
fn production_avatar_keep_books_held_item_as_inventory() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (define box
                    (make-map "actor" "did:ma:world#box" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Box"))
                (define crate
                    (make-map "actor" "did:ma:world#crate" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Crate"))
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Lamp"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#box" box
                                                   "did:ma:world#crate" crate
                                                   "did:ma:world#lamp" lamp)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#lamp")
                                (equal? method "kind?")) "thing")
                          ((equal? method "kind?") "/ma/container/0.0.1")
                          ((equal? method "contents?") ())
                          (else ())))

                ; keep with an empty hand errors.
                (guard (e ((string-contains e "not holding anything") #t)
                          (#t (error (string-append "expected empty-hand error, got: " e))))
                    (keep)
                    (error "keep did not refuse"))

                ; take then keep: held -> inventory slot, hand freed, and keep
                ; itself sends no pick-up RPC.
                (smoke "take" (lambda () (take "Box")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#box"
                                                      "parent" "did:ma:me"
                                                      "name" "Box")))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#box"))
                (set! calls ())
                (smoke "keep" (lambda () (keep)))
                (assert (equal? calls (list (list "did:ma:world#box" "kind?" ()))))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#box"))
                (assert (null? (hand-pool)))

                ; keep by name drops the previous inventory container.
                (smoke "take crate" (lambda () (take "Crate")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#crate"
                                                      "parent" "did:ma:me"
                                                      "name" "Crate")))
                (set! calls ())
                (smoke "keep crate" (lambda () (keep "Crate")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#crate"))
                (assert (null? (hand-pool)))
                (let ((r (reverse calls)))
                    (assert (equal? (car r)
                        (list "did:ma:world#box" "set-parent"
                            (list "did:ma:world#room"))))
                    (assert (equal? (cadr r)
                        (list "did:ma:world#room" "drop"
                            (list "did:ma:world#box"))))
                    (assert (equal? (caddr r)
                        (list "did:ma:world#crate" "kind?" ()))))

                ; keeping a non-container is refused.
                (smoke "take lamp" (lambda () (take "Lamp")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me"
                                                      "name" "Lamp")))
                (set! calls ())
                (guard (e ((string-contains e "invalid inventory kind") #t)
                          (#t (error (string-append "expected kind refusal, got: " e))))
                    (keep)
                    (error "keep did not refuse"))
                (assert (equal? calls (list (list "did:ma:world#lamp" "kind?" ()))))
                "keep-slot-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "keep-slot-ok");
}

#[test]
fn production_avatar_take_from_equipped_inventory_never_drops_it() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (define vadsaek
                    (make-map "actor" "did:ma:world#vadsaek" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Vadsæk"))
                (define bag
                    (make-map "actor" "did:ma:world#bag" "kind" "thing"
                              "parent" "did:ma:world#vadsaek" "name" "Bag"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#vadsaek" vadsaek)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#vadsaek")
                                (equal? method "contents?")) (list bag))
                          ((equal? method "kind?") "/ma/container/0.0.1")
                          (else ())))

                ; take the vadsæk from the room, then equip it: the container
                ; moves from the hand slot to the inventory slot.
                (set! calls ())
                (smoke "take" (lambda () (take "Vadsæk")))
                (assert (equal? calls (list (list "did:ma:world#vadsaek" "hold" ()))))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#vadsaek"
                                                      "parent" "did:ma:me"
                                                      "name" "Vadsæk")))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#vadsaek"))
                (set! calls ())
                (smoke "equip" (lambda () (equip "Vadsæk")))
                (assert (equal? calls
                    (list (list "did:ma:world#vadsaek" "kind?" ())
                          (list "did:ma:world#vadsaek" "hold" ()))))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#vadsaek"
                                                      "parent" "did:ma:me"
                                                      "name" "Vadsæk")))
                ; the equipped container left the hand slot and is booked.
                (assert (null? (hand-pool)))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#vadsaek"))

                ; a re-proposal of the equipped container (e.g. a repeated
                ; :hold racing the booking) never lands in the hand slot.
                (on-event ":parent" (list (make-map "actor" "did:ma:world#vadsaek"
                                                      "parent" "did:ma:me"
                                                      "name" "Vadsæk")))
                (assert (null? (hand-pool)))

                ; taking the bag out of the equipped vadsæk drops nothing: the
                ; only relocation call is the container's :take gate.
                (set! calls ())
                (smoke "take from" (lambda () (take "Bag" "from" "Vadsæk")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#vadsaek" "take" (list "did:ma:world#bag"))))
                (assert (not (member? (list "did:ma:world#room" "drop"
                                            (list "did:ma:world#vadsaek")) calls)))
                (assert (not (member? (list "did:ma:world#vadsaek" "set-parent"
                                            (list "did:ma:world#room")) calls)))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#vadsaek"))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#bag"
                                                      "parent" "did:ma:me"
                                                      "name" "Bag")))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#bag"))

                ; defence-in-depth: a stale hand slot naming the inventory
                ; container never drops it nor clears the pointer.
                (set! held-item vadsaek)
                (set! calls ())
                (smoke "take from stale" (lambda () (take "Bag" "from" "Vadsæk")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#vadsaek" "take" (list "did:ma:world#bag"))))
                (assert (not (member? (list "did:ma:world#room" "drop"
                                            (list "did:ma:world#vadsaek")) calls)))
                (assert (not (member? (list "did:ma:world#vadsaek" "set-parent"
                                            (list "did:ma:world#room")) calls)))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#vadsaek"))
                "equip-take-from-safe"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "equip-take-from-safe");
}

#[test]
fn production_avatar_equip_keeps_unrelated_held_item() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Lamp"))
                (define crate
                    (make-map "actor" "did:ma:world#crate" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Crate"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#lamp" lamp
                                                   "did:ma:world#crate" crate)))
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#lamp")
                                (equal? method "kind?")) "thing")
                          ((equal? method "kind?") "/ma/container/0.0.1")
                          ((equal? method "contents?") ())
                          (else ())))

                ; hold the lamp, then equip a different container: only the
                ; booked container leaves the hand slot.
                (smoke "take lamp" (lambda () (take "Lamp")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me"
                                                      "name" "Lamp")))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#lamp"))
                (smoke "equip crate" (lambda () (equip "Crate")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#crate"
                                                      "parent" "did:ma:me"
                                                      "name" "Crate")))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#crate"))
                (assert (equal? (entry-actor (car (hand-pool))) "did:ma:world#lamp"))
                "equip-keeps-held"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "equip-keeps-held");
}

const OBJECT_CMDS_PREAMBLE: &str = r#"
                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (#.my.ctx.inv: "did:ma:world#inventory")
                (#.my.ctx.nick: "tester")

                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Lamp"))
                (define vase
                    (make-map "actor" "did:ma:world#vase" "kind" "thing"
                              "parent" "did:ma:world#room" "name" "Vase"))
                (define box
                    (make-map "actor" "did:ma:world#box" "kind" "container"
                              "parent" "did:ma:world#room" "name" "Box"))
                (define coin
                    (make-map "actor" "did:ma:world#coin" "kind" "thing"
                              "parent" "did:ma:world#inventory" "name" "Coin"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:world#lamp" lamp
                                                   "did:ma:world#vase" vase
                                                   "did:ma:world#box" box)))

                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#inventory")
                                (equal? method "kind?"))
                           "/ma/container/0.0.1")
                          ((and (equal? actor "did:ma:world#inventory")
                                (equal? method "contents?"))
                           (list coin))
                          ((and (equal? actor "did:ma:world#box")
                                (equal? method "contents?"))
                           (list coin))
                          ((equal? method "contents?") ())
                          ((equal? method "owner?") "did:ma:owner")
                          (else ())))
"#;

fn object_commands_test_source(body: &str) -> String {
    let libs = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    format!("{libs}\n{OBJECT_CMDS_PREAMBLE}\n{body}")
}

#[test]
fn production_avatar_take_variants_resolve_and_route() {
    let source = object_commands_test_source(
        r#"
                ; take from the room → :hold directly on the resolved actor.
                (set! calls ())
                (smoke "take room" (lambda () (take "Lamp")))
                (assert (equal? calls
                    (list (list "did:ma:world#lamp" "hold" ()))))

                ; taking a second thing while already holding drops the current one first.
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me")))
                (set! calls ())
                (smoke "take while holding" (lambda () (take "Vase")))
                (let ((r (reverse calls)))
                    (assert (equal? (car r) (list "did:ma:world#vase" "hold" ())))
                    (assert (equal? (cadr r)
                        (list "did:ma:world#lamp" "set-parent" (list "did:ma:world#room"))))
                    (assert (equal? (caddr r)
                        (list "did:ma:world#room" "drop" (list "did:ma:world#lamp")))))
                (held-clear!)

                ; take from a container → the container's :take.
                (set! calls ())
                (smoke "take from container" (lambda () (take "Coin" "from" "Box")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#box" "take" (list "did:ma:world#coin"))))
                (held-clear!)

                ; take-from is the positional alias for the same verb.
                (set! calls ())
                (smoke "take-from" (lambda () (take-from "Box" "Coin")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#box" "take" (list "did:ma:world#coin"))))
                "take-variants-ok"
        "#,
    );
    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "take-variants-ok");
}

#[test]
fn production_avatar_put_and_drop_route_to_actors() {
    let source = object_commands_test_source(
        r#"
                ; put into a container → the item's :put.
                (set! calls ())
                (smoke "put" (lambda () (put "Coin" "in" "Box")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#coin" "put" (list "did:ma:world#box"))))
                (held-clear!)

                ; drop a held item (no argument).
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me")))
                (set! calls ())
                (smoke "drop" (lambda () (drop)))
                (let ((r (reverse calls)))
                    (assert (equal? (car r)
                        (list "did:ma:world#coin" "set-parent" (list "did:ma:world#room"))))
                    (assert (equal? (cadr r)
                        (list "did:ma:world#room" "drop" (list "did:ma:world#coin")))))
                (held-clear!)

                ; drop a held item by name.
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me"
                                                      "name" "Coin")))
                (set! calls ())
                (smoke "drop by name" (lambda () (drop "Coin")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#coin" "set-parent" (list "did:ma:world#room"))))
                "put-drop-ok"
        "#,
    );
    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "put-drop-ok");
}

#[test]
fn production_avatar_recycle_routes_to_owned_actor() {
    let source = object_commands_test_source(
        r#"
                ; recycle an owned object.
                (set! calls ())
                (smoke "recycle" (lambda () (recycle "Lamp")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "recycle" ())))
                (held-clear!)

                ; recycle an object inside a container.
                (set! calls ())
                (smoke "recycle-from" (lambda () (recycle-from "Box" "Coin")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#coin" "recycle" ())))
                "recycle-ok"
        "#,
    );
    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "recycle-ok");
}

#[test]
fn production_avatar_prop_setters_are_command_sugar() {
    let source = object_commands_test_source(
        r#"
                ; name/describe/nick are prop sugar.
                (set! calls ())
                (smoke "name" (lambda () (name "Lamp" "as" "Desk Lamp")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "prop" (list "name" "Desk Lamp"))))
                (set! calls ())
                (smoke "describe" (lambda () (describe "Lamp" "as" "A warm lamp.")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "prop" (list "description" "A warm lamp."))))
                (set! calls ())
                (smoke "nick" (lambda () (nick "Lamp" "as" "The Lamp")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "prop" (list "nick" "The Lamp"))))
                "prop-sugar-ok"
        "#,
    );
    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "prop-sugar-ok");
}

#[test]
fn production_avatar_owner_and_claim_route_to_actor() {
    let source = object_commands_test_source(
        r#"
                ; owner and claim route to the resolved actor's RPC.
                (set! calls ())
                (smoke "owner" (lambda () (owner "Lamp")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "owner?" ())))
                (set! calls ())
                (smoke "claim" (lambda () (claim "Lamp")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "claim" ())))
                (set! calls ())
                (smoke "claim secret" (lambda () (claim "Lamp" "hunter2")))
                (assert (equal? (car (reverse calls))
                    (list "did:ma:world#lamp" "claim" (list "hunter2"))))
                "owner-claim-ok"
        "#,
    );
    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "owner-claim-ok");
}

#[test]
fn production_avatar_go_traverses_exit_and_enters_target() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (#.my.ctx.inv: "did:ma:world#inventory")
                (#.my.ctx.nick: "tester")

                (define hull
                    (make-map "actor" "did:ma:world#exit-hull" "kind" "exit"
                              "parent" "did:ma:world#room" "direction" "hull"
                              "name" "Hull"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room" "name" "Room"
                              "children" (make-map "did:ma:world#exit-hull" hull)))

                (define calls ())
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#inventory")
                                (equal? method "kind?"))
                           "/ma/container/0.0.1")
                          ((equal? method "contents?") ())
                          ((equal? method "traverse")
                           (make-map "parent" "did:ma:world#elsewhere"
                                     "text" "You go hull."))
                          ((equal? method "enter")
                           (make-map "parent" "did:ma:world#elsewhere"
                                     "nick" "tester"))
                          (else ())))

                (go "hull")

                ; First hop: ask the exit to traverse with our own did and parent.
                (assert (equal? (car calls)
                    (list "did:ma:world#exit-hull" "traverse"
                          (list (make-map "did" "did:ma:me"
                                          "parent" "did:ma:world#room")))))
                ; Second hop: enter the room the exit returned.
                (assert (equal? (cadr calls)
                    (list "did:ma:world#elsewhere" "enter" (list "tester"))))
                ; The room address was updated from the entry reply.
                (assert (equal? (#.my.ctx.room) "did:ma:world#elsewhere"))
                "avatar-go-ok"
        "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-go-ok");
}

#[test]
fn production_split_command_parses_reserved_keyword_slots() {
    let source = ["stdlib", "avatar"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                ; Required keyword with before and after.
                (assert (equal? (split-command! (list "lampe" "in" "vadsæk") "in" "usage" #t)
                                (list (list "lampe") (list "vadsæk"))))
                ; Optional keyword absent leaves after as #f.
                (assert (equal? (split-command! (list "lampe") "from" "usage" #f)
                                (list (list "lampe") #f)))
                ; Optional keyword present captures after.
                (assert (equal? (split-command! (list "lampe" "from" "vadsæk") "from" "usage" #f)
                                (list (list "lampe") (list "vadsæk"))))

                ; Errors: empty before, missing required keyword, empty after.
                (assert (equal? (guard (e (#t "raised")) (split-command! () "in" "usage" #t) "returned")
                                "raised"))
                (assert (equal? (guard (e (#t "raised")) (split-command! (list "lampe") "in" "usage" #t) "returned")
                                "raised"))
                (assert (equal? (guard (e (#t "raised")) (split-command! (list "lampe" "in") "in" "usage" #t) "returned")
                                "raised"))
                ; Optional keyword present but empty after is also an error
                ; (take <item> from / forge <kind> named <name> in).
                (assert (equal? (guard (e (#t "raised")) (split-command! (list "lampe" "from") "from" "usage" #f) "returned")
                                "raised"))
                "split-command-ok"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "split-command-ok");
}

#[test]
fn production_forge_rejects_did_name() {
    let source = ["stdlib", "runtime", "avatar"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                ; A name must never be a DID or DID-URL: the resolver treats a
                ; leading did:ma: as an address, so a name shaped like one is
                ; unresolvable. Both bare and fragmented forms are rejected.
                (assert (equal? (guard (e (#t "raised"))
                                  (forge "thing" "named" "did:ma:world#lamp" "in" "did:ma:world#inventory")
                                  "returned")
                                "raised"))
                (assert (equal? (guard (e (#t "raised"))
                                  (forge "thing" "named" "did:ma:world" "in" "did:ma:world#inventory")
                                  "returned")
                                "raised"))
                "forge-did-name-ok"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "forge-did-name-ok");
}

#[test]
fn production_find_takes_a_hidden_object_only_when_within_reach() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:me")
                (#.my.ctx.room: "did:ma:world#room")
                (#.my.ctx.inv: "did:ma:world#inventory")
                (define calls ())
                (define (smoke name thunk)
                    (guard (failure (#t (error (string-append name ": " failure))))
                        (thunk)))
                (define (actor-call actor method . args)
                    (set! calls (append calls (list (list actor method args))))
                    (cond ((and (equal? actor "did:ma:world#duckie") (equal? method "parent?"))
                           "did:ma:world#room")
                          ((and (equal? actor "did:ma:world#duckie") (equal? method "name"))
                           "Duckie")
                          ((and (equal? actor "did:ma:world#ghost") (equal? method "parent?"))
                           "did:ma:world#elsewhere")
                          (else ())))

                (set! calls ())
                (smoke "find" (lambda () (find "did:ma:world#duckie")))
                (assert (equal? calls
                    (list (list "did:ma:world#duckie" "parent?" ())
                          (list "did:ma:world#duckie" "hold" ())
                          (list "did:ma:world#duckie" "name" ()))))

                (set! calls ())
                (define found-elsewhere?
                    (guard (failure (#t #t)) (find "did:ma:world#ghost") #f))
                (assert found-elsewhere?)
                "find-ok"
        "#
    );

    let (value, ctx) = eval(&source).unwrap();
    assert_eq!(value.display(), "find-ok");
    assert_eq!(ctx.output.borrow().as_str(), "You found Duckie.");
}

#[test]
fn production_avatar_resolves_room_and_inventory_children_or_reports_ambiguity() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.ctx.room: "did:ma:world#room")
                (define direct-attila
                    (make-map "actor" "did:ma:attila" "did" "did:ma:attila"
                                        "kind" "agent" "name" "Attila" "nick" "Attila"
                                        "description" "A direct DID presence."))
                (define agent-attila
                    (make-map "actor" "did:ma:world#attila" "name" "Attila"
                                        "nick" "Attila" "kind" "agent"
                                        "description" "An agent."))
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "kind" "thing" "name" "Lamp"
                                        "description" "A lamp."))
                (define mirror
                    (make-map "actor" "did:ma:world#mirror" "kind" "exit" "name" "Mirror"
                                        "direction" "mirror" "description" "An exit."))
                (define inventory-coin
                    (make-map "actor" "did:ma:world#coin" "kind" "thing" "name" "Coin"
                                        "description" "An inventory coin."))
                (define room-duckie
                    (make-map "actor" "did:ma:world#room-duckie"
                                        "kind" "thing"
                                        "parent" "did:ma:world#room"
                                        "name" "Rubber Duckie" "nick" "Duckie"))
                (define inventory-duckie
                    (make-map "actor" "did:ma:world#inventory-duckie"
                                        "kind" "thing"
                                        "parent" "did:ma:world#inventory"
                                        "name" "Rubber Duckie" "nick" "Duckie"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room" "name" "The Construct"
                                        "children" (make-map "did:ma:attila" direct-attila
                                                             "did:ma:world#attila" agent-attila
                                                             "did:ma:world#lamp" lamp
                                                             "did:ma:world#room-duckie" room-duckie
                                                             "did:ma:world#mirror" mirror)))
                (#.my.ctx.inv: "did:ma:world#inventory")

                (define (actor-call actor method . params)
                    (cond ((and (equal? actor "did:ma:world#inventory")
                              (equal? method "kind?"))
                          "/ma/container/0.0.1")
                         ((and (equal? actor "did:ma:world#inventory")
                              (equal? method "contents?"))
                          (list inventory-coin inventory-duckie))
                         (else actor)))

                (assert (equal? (command "Lamp" "probe") "did:ma:world#lamp"))
                (assert (equal? (command "Mirror" "probe") "did:ma:world#mirror"))
                (assert (equal? (command "Coin" "probe") "did:ma:world#coin"))

                (guard (failure
                        ((and (string-contains failure "matches more than one")
                              (string-contains failure "did:ma:attila")
                              (string-contains failure "did:ma:world#attila")) #t)
                        (#t (error failure)))
                    (command "Attila" "probe"))

                (guard (failure
                        ((and (string-contains failure
                                "Rubber Duckie \"Duckie\" (did:ma:world#room-duckie) in The Construct")
                              (string-contains failure
                                "Rubber Duckie \"Duckie\" (did:ma:world#inventory-duckie) in inventory")) #t)
                        (#t (error failure)))
                    (command "Duckie" "probe"))

                (assert (equal?
                    (describe-candidate
                        (make-map "actor" "did:ma:world#boxed-duckie"
                                  "parent" "did:ma:world#box"
                                  "name" "Rubber Duckie" "nick" "Duckie"))
                    "Rubber Duckie \"Duckie\" (did:ma:world#boxed-duckie)"))

                (set! last-room
                    (make-map "actor" "did:ma:world#room" "name" "The Construct"
                                        "children" (make-map "did:ma:attila" direct-attila
                                                             "did:ma:world#lamp" lamp
                                                             "did:ma:world#mirror" mirror)))
                (look "Coin")
                "room-child-resolver-ok"
                "#
    );

    let (value, test_ctx) = eval(&source).unwrap();
    assert_eq!(value.display(), "room-child-resolver-ok");
    assert_eq!(
        test_ctx.output.borrow().as_str(),
        "Coin\nAn inventory coin."
    );
}

#[test]
fn production_avatar_give_sends_a_claim_offer_to_one_person() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
                {source}

                (#.my.identity.did: "did:ma:bob")
                (define bob
                    (make-map "did" "did:ma:bob" "actor" "did:ma:bob"
                              "kind" "agent" "name" "Bob" "nick" "Bob"))
                (define alice
                    (make-map "did" "did:ma:alice" "actor" "did:ma:alice"
                              "kind" "agent" "name" "Alice" "nick" "Alice"))
                (define duckie
                    (make-map "actor" "did:ma:world#duckie"
                              "kind" "thing" "name" "Rubber Duckie" "nick" "Duckie"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "children" (make-map "did:ma:bob" bob
                                                   "did:ma:alice" alice
                                                   "did:ma:world#duckie" duckie)))

                (define call-order "")
                (define called-actor "")
                (define called-method "")
                (define called-args ())
                (define sent-to "")
                (define sent-body "")
                (define (random n) 123456789)
                (define (actor-call actor method . args)
                    (set! call-order (string-append call-order "rpc "))
                    (set! called-actor actor)
                    (set! called-method method)
                    (set! called-args args)
                    nil)
                (define (msg-send target body)
                    (set! call-order (string-append call-order "msg"))
                    (set! sent-to target)
                    (set! sent-body body)
                    (list ":ok" "message-1"))

                (give "Duckie" "to" "Alice")
                (assert (equal? call-order "rpc msg"))
                (assert (equal? called-actor "did:ma:world#duckie"))
                (assert (equal? called-method "set-recovery-secret"))
                (assert (equal? called-args (list "123456789-123456789-123456789")))
                (assert (equal? sent-to "did:ma:alice"))
                (assert (string-contains sent-body "Bob wants to give you Duckie."))
                (assert (string-contains sent-body
                    "claim did:ma:world#duckie 123456789-123456789-123456789"))

                (set! call-order "")
                (claim "did:ma:world#duckie" "123456789-123456789-123456789")
                (assert (equal? called-actor "did:ma:world#duckie"))
                (assert (equal? called-method "claim"))
                (assert (equal? called-args (list "123456789-123456789-123456789")))

                (set! call-order "")
                (guard (failure
                        ((string-contains failure "yourself") #t)
                        (#t (error failure)))
                    (give "Duckie" "to" "Bob"))
                (assert (equal? call-order ""))

                (guard (failure
                        ((string-contains failure "not a person") #t)
                        (#t (error failure)))
                    (give "Duckie" "to" "did:ma:world#other-duckie"))
                (assert (equal? call-order ""))
                "avatar-give-ok"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-give-ok");
}

#[test]
fn production_libraries_compose_in_order() {
    let source = ["stdlib", "runtime", "avatar", "events"]
        .into_iter()
        .map(|name| fs::read_to_string(format!("lib/{name}.zscheme")).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("{source}\n(on-event \":print\" (list \"hello\"))\n\"loaded\"");

    let (value, test_ctx) = eval(&source).unwrap();
    assert_eq!(value.display(), "loaded");
    assert_eq!(test_ctx.output.borrow().as_str(), "hello");
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
        (#.my.aliases.sky: "did:ma:sky")
        (#.my.aliases.ms: "did:ma:ms")
        (#.my.aliases)
    "#;

    let (value, _) = eval(source).unwrap();
    assert_eq!(value.display(), "(\".my.aliases.ms\" \".my.aliases.sky\")");
}

#[test]
fn unit_dot_alias_storage_feeds_target_resolution() {
    let source = r#"
        (#.my.aliases.sky: "did:ma:sky")
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
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zscheme"))
        .collect();

    let functional_dir = Path::new("tests/functional");
    if functional_dir.exists() {
        paths.extend(
            fs::read_dir(functional_dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zscheme")),
        );
    }

    paths.sort();
    assert!(
        !paths.is_empty(),
        "expected at least one functional .zscheme test"
    );

    for path in paths {
        eval_file(&path).unwrap_or_else(|err| panic!("{} failed: {err}", path.display()));
    }
}
