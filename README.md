# zscheme - Structure and Implementation of Systemic Chaos™

Scheme scripting for the [間 (ma)](https://github.com/bahner/ma) distributed
actor platform.

---

## What is zscheme

zscheme is a Lisp/Scheme dialect embedded in `zion`, the browser-based
`did:ma:` actor workstation. It lets you compose, automate, and script
interactions with the distributed actor network using standard Scheme syntax.

Any command line in `zion` containing `(…)` is pre-processed by the evaluator
before normal dispatch. Results are spliced back as strings into the command.

```scheme
; Inline substitution — result becomes part of the command
(.my.aliases.sky)#room:enter ((.my.aliases.ms)#house:enter #room)

; Standalone expressions
(+ 7 5)                               ; → 12
(define (square x) (* x x))
(square 9)                            ; → 81
```

## Key features

- **Distributed primitives** — call any `did:ma:` actor directly from Scheme
- **Session environment** — definitions persist across the login session
- **`|` pipe threading** — compose RPC results and Scheme functions in a pipeline
- **Scriptable docs** — store scripts in any `.my` path, share via IPFS CID
- **`.my.scheme!save`** — serialise your session env to a persistent image
- **Stdlib** — common functions in pure zscheme, loadable from IPFS
- **`include`** — load a script by path: `(include ".my.scheme")`

---

## Backend daemon

The `zscheme` identity has exactly one iroh endpoint on the network, so only
one process may own it at a time. To allow concurrent REPLs and scripts,
`zscheme` runs as a thin client by default: it connects to a per-user backend
daemon over a Unix socket (`$XDG_RUNTIME_DIR/zscheme.sock`) and submits Scheme
source for evaluation. The daemon is auto-spawned on first use and owns the
secret bundle, the iroh endpoint, and the shared session environment — so
`(define …)` in one REPL is visible in every other client.

| Flag | Meaning |
|------|---------|
| *(none)* | Client mode — auto-spawns the daemon if needed |
| `daemon [--img FILE]` | Run the backend daemon in the foreground; replaces a running daemon. `--img` loads a session image at startup and saves it on shutdown |
| `stop` | Stop the running daemon |
| `reset` | Reset the shared session environment (drop all defines) |
| `save [FILE]` | Save the session environment as Scheme source (stdout or FILE) — reload with `zscheme FILE` |
| `--isolated` | Use a fresh per-connection environment instead of the shared one |
| `standalone [script]` | Old in-process mode (own endpoint, no daemon) — only one at a time |

The auto-spawned daemon inherits `MA_SECRET_BUNDLE_PASSPHRASE` from the
client's environment and logs to `~/.local/share/ma/zscheme-daemon.log`.
It runs until logout/reboot or `zscheme --stop`.

---

## Quick start

### Arithmetic and strings

```scheme
(+ 1 2)                               ; → 3
(string-append "hello" " " "world")   ; → hello world
(string-length "did:ma:")             ; → 7
```

### Defining functions

```scheme
(define (fib n)
  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))

(fib 10)                              ; → 55
```

### Config lookups

```scheme
(.my.aliases.sky)                      ; returns stored DID
(.my.config.colour.text)               ; returns colour string
(.my.config.k: "value")               ; sets a config key
```

### Actor RPC

```scheme
; @ syntax — auto-unwraps the reply value:
(@sky#house:enter #room)              ; → "ticket-xyz"

; rpc-send — returns a raw (:ok …) / (:error …) tuple:
(rpc-send "@sky#house" ":enter" "#room")  ; → (:ok "ticket-xyz")
(ok? (rpc-send "@sky#ping" ":ping"))      ; → #t
```

### Entering a world

```scheme
(include ".my.doc.stdlib.ma")

(define (enter-world addr)
  (let* ((at      (string-index addr "@"))
         (hash    (string-index addr "#"))
         (alias   (substring addr 0 at))
         (runtime (string-append "@" (substring addr (+ at 1) hash)))
         (room    (substring addr hash (string-length addr)))
         (target  (string-append runtime room))
         (_       (rpc-send (string-append runtime "#avatar") ":claim" alias))
         (result  (rpc-send (string-append runtime "#house") ":enter" room)))
    (if (ok? result)
        (let ((entered (rpc-send target ":enter" (ok-val result))))
          (if (ok? entered)
              (begin (use target) (ok-val entered))
              (error (err-msg entered))))
        (error (err-msg result)))))

; Usage:
; (enter-world "alice@sky#room")
```

---

## Pipe threading

Inside `(…)` expressions, `|` threads a value through a chain of functions:

```scheme
(@sky#room:who | (search-by "hans") | length)
; → how many users named "hans"

(@sky#room:inventory | string-lines | (take 10))
; → first 10 lines of inventory

; Use _ as explicit placeholder:
(@sky#room:who | (take _ 5) | (join _ "\n"))
```

---

## Session image

Save your definitions between sessions:

```
.my.scheme!save   ; serialise session env to .my.scheme.content
.my.scheme!edit   ; review and clean up
.my.scheme!eval   ; reload after editing
```

Auto-load at login:

```zscheme
.my.scheme.autoload: true
```

---

## Loading the stdlib

The stdlib (`stdlib.ma`) provides list helpers such as `map`, `filter`,
`fold`, `take`, `drop`, `member`, and `contains?`; string helpers such as
`string-split` and `string-join`; and associative map helpers such as
`make-map`, `map-ref`, `map-set`, `map-delete`, `map-keys`, `map-values`,
`map->alist`, and `alist->map`.

```
; In zion:
.my.doc.stdlib.ma!fetch /ipfs/<cid>   ; fetch from IPFS by CID
.my.doc.stdlib.ma!eval                ; evaluate into session environment

; From inside a Scheme expression:
(include ".my.doc.stdlib.ma")
```

---

## This repository

| File | Description |
|---|---|
| [`stdlib.ma`](stdlib.ma) | Standard library — pure zscheme implementations |
| [`REFERENCE.md`](REFERENCE.md) | Complete language reference |
| [`HANDBOOK.md`](HANDBOOK.md) | Practical user handbook |

The formal specification lives in the ma-spec repository:
[zscheme-v1.md](https://github.com/bahner/ma-spec/blob/main/zscheme/zscheme-v1.md).
