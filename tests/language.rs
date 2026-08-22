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
                              "who" (list (make-map "name" "Alice"))
                              "agents" (list (make-map "name" "Alice")
                                             (make-map "name" "Attila"))
                              "exits" (list (make-map "direction" "mirror"))))
                "#
    );

    let (_, ctx) = eval(&source).unwrap();
    assert_eq!(
        ctx.output.borrow().as_str(),
        "Atrium\nQuiet.\nOccupants:\nAlice\nAttila\nExits:\nmirror"
    );
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
                    (make-map "exits"
                        (list (make-map "actor" "did:ma:exit-hull" "direction" "hull"))))
                (assert (equal? (resolve-exit "hull") "did:ma:exit-hull"))

                (set! last-room
                    (make-map "exits"
                        (list (make-map "actor" "did:ma:exit-one" "direction" "east hatch")
                                    (make-map "actor" "did:ma:exit-two" "direction" "east door"))))
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
fn production_avatar_derives_root_when_creating_first_inventory() {
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
                (assert (equal? (my-inv) "did:ma:world#inventory"))
                (assert (equal? forge-target "did:ma:world#root"))
                (assert (equal? (#.my.ctx.inv) "did:ma:world#inventory"))

                (#.my.ctx.inv: "did:ma:elsewhere#travelling-bag")
                (set! forge-target #f)
                (assert (equal? (my-inv) "did:ma:elsewhere#travelling-bag"))
                (assert (equal? forge-target #f))
                "avatar-root-derived"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-root-derived");
}

#[test]
fn production_avatar_commands_accept_representative_arguments() {
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
                (#.my.ctx.nick: "tester")
                (#.my.ctx.inv: "did:ma:world#inventory")

                (define lamp (make-map "actor" "did:ma:world#lamp" "name" "Brass Lamp"))
                (define box (make-map "actor" "did:ma:world#box" "name" "Wooden Box"))
                (define coin (make-map "actor" "did:ma:world#coin" "name" "Silver Coin"))
                (define hull-exit
                    (make-map "actor" "did:ma:world#exit-hull" "direction" "hull"))
                (define room-snapshot
                    (make-map "actor" "did:ma:world#room" "parent" "did:ma:world#room"
                                        "nick" "tester" "name" "Test room" "description" "Ready."
                                        "who" () "agents" () "things" (list lamp)
                                        "exits" (list hull-exit)))
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
                          ((equal? method "look")
                                 (if (equal? actor "did:ma:world#inventory")
                                     (make-map "things" (list coin))
                                     room-snapshot))
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

                (smoke "go" (lambda () (go "hull")))
                (smoke "dig" (lambda () (dig "east")))
                (smoke "fill" (lambda () (fill "east")))
                (smoke "forge" (lambda () (forge "thing" "named" "Test Thing")))
                (smoke "leave" (lambda () (leave)))
                (smoke "enter" (lambda () (enter "did:ma:world#room" "tester")))

                (smoke "hold" (lambda () (hold "lamp")))
                (assert (equal? (car (car (reverse actor-calls))) "did:ma:world#lamp"))
                (assert (equal? (car (cdr (car (reverse actor-calls)))) "hold"))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me")))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#lamp"
                                                      "parent" "did:ma:me")))
                (set! actor-calls ())
                (smoke "hold next item" (lambda () (hold "coin")))
                (assert (equal? (car (car (reverse actor-calls))) "did:ma:world#coin"))
                (assert (equal? (car (cdr (car (reverse actor-calls)))) "hold"))
                (smoke "take from container" (lambda () (take "Silver" "Coin" "from" "Wooden" "Box")))
                (assert (equal? (car (car (reverse actor-calls))) "did:ma:world#coin"))
                (assert (equal? (car (cdr (car (reverse actor-calls)))) "hold"))
                (on-event ":parent" (list (make-map "actor" "did:ma:world#coin"
                                                      "parent" "did:ma:me")))
                (smoke "put held item" (lambda () (put "coin" "in" "box")))
                (assert (equal? (car (car (reverse actor-calls))) "did:ma:world#coin"))
                (assert (equal? (car (cdr (car (reverse actor-calls)))) "set-parent"))
                (smoke "recycle-from" (lambda () (recycle-from "box" "coin")))
                (smoke "roll-call" (lambda () (roll-call "box")))
                (smoke "say" (lambda () (say "hello" "world")))
                (smoke "emote" (lambda () (emote "waves")))
                (smoke "claim" (lambda () (claim "lamp")))
                (smoke "owner" (lambda () (owner "lamp")))
                (smoke "recycle" (lambda () (recycle "lamp")))
                (smoke "remove" (lambda () (remove "stale" "child")))
                (smoke "tell" (lambda () (tell "lamp" "to" "ping" "once")))

                (smoke "here?" (lambda () (here?)))
                (smoke "who?" (lambda () (who?)))
                (smoke "occupants?" (lambda () (occupants?)))
                (smoke "things?" (lambda () (things?)))
                (smoke "exits?" (lambda () (exits?)))
                (smoke "inv set" (lambda () (inv "did:ma:world#inventory")))
                (smoke "inv show" (lambda () (inv)))
                (smoke "help" (lambda () (help)))
                (smoke "help target" (lambda () (help "lamp")))
                (smoke "look" (lambda () (look)))
                (smoke "look target" (lambda () (look "lamp")))
                "avatar-commands-ok"
                "#
    );

    let (value, _) = eval(&source).unwrap();
    assert_eq!(value.display(), "avatar-commands-ok");
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

                (define direct-attila
                    (make-map "actor" "did:ma:attila" "did" "did:ma:attila"
                                        "name" "Attila" "nick" "Attila"
                                        "description" "A direct DID presence."))
                (define agent-attila
                    (make-map "actor" "did:ma:world#attila" "name" "Attila"
                                        "nick" "Attila" "kind" "agent"
                                        "description" "An agent."))
                (define lamp
                    (make-map "actor" "did:ma:world#lamp" "name" "Lamp"
                                        "description" "A lamp."))
                (define mirror
                    (make-map "actor" "did:ma:world#mirror" "name" "Mirror"
                                        "direction" "mirror" "description" "An exit."))
                (define inventory-coin
                    (make-map "actor" "did:ma:world#coin" "name" "Coin"
                                        "description" "An inventory coin."))
                (define room-duckie
                    (make-map "actor" "did:ma:world#room-duckie"
                                        "parent" "did:ma:world#room"
                                        "name" "Rubber Duckie" "nick" "Duckie"))
                (define inventory-duckie
                    (make-map "actor" "did:ma:world#inventory-duckie"
                                        "parent" "did:ma:world#inventory"
                                        "name" "Rubber Duckie" "nick" "Duckie"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room" "name" "The Construct"
                                        "who" (make-map "did:ma:attila" direct-attila)
                                        "agents" (list agent-attila)
                                        "things" (list lamp room-duckie)
                                        "exits" (list mirror)))
                (#.my.ctx.inv: "did:ma:world#inventory")

                (define (actor-call actor method . params)
                    (if (and (equal? actor "did:ma:world#inventory")
                             (equal? method "contents?"))
                        (list inventory-coin inventory-duckie)
                        actor))

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
                                        "who" (make-map "did:ma:attila" direct-attila)
                                        "agents" () "things" (list lamp) "exits" (list mirror)))
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
                              "name" "Bob" "nick" "Bob"))
                (define alice
                    (make-map "did" "did:ma:alice" "actor" "did:ma:alice"
                              "name" "Alice" "nick" "Alice"))
                (define duckie
                    (make-map "actor" "did:ma:world#duckie"
                              "name" "Rubber Duckie" "nick" "Duckie"))
                (set! last-room
                    (make-map "actor" "did:ma:world#room"
                              "who" (list bob alice) "agents" ()
                              "things" (list duckie) "exits" ()))

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
