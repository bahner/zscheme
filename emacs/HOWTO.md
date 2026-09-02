# ma.el HOWTO

This directory contains the Emacs client for `zscheme`. It is intentionally
thin: Emacs connects to the existing `zscheme` daemon, sends zscheme source over
the same socket protocol as the CLI client, and receives normal results back.

## Installation

For development, add this directory to your Emacs `load-path` and require the
thin API:

```elisp
(add-to-list 'load-path "/home/bahner/src/zscheme/emacs")
(require 'ma)
(require 'ma-mode)
(require 'ma-edit)
```

With `use-package`, the same setup can be written as:

```elisp
(use-package ma
  :load-path "/home/bahner/src/zscheme/emacs")

(use-package ma-mode
  :load-path "/home/bahner/src/zscheme/emacs"
  :after ma
  :mode "\\.zcm\\'")

(use-package ma-edit
  :load-path "/home/bahner/src/zscheme/emacs"
  :after ma)
```

No external Elisp packages are required for the initial client. The CBOR codec
used for the zscheme daemon IPC protocol is included in `ma-ipc.el`.

After `(require 'ma-mode)`, files ending in `.zcm` open in `ma-mode` by
default.

## First connection

Make sure `zscheme` is on your `PATH`, or set `ma-ipc-program` before loading:

```elisp
(setq ma-ipc-program "/home/bahner/src/zscheme/target/debug/zscheme")
```

Then connect:

```elisp
(ma-connect)
```

If the daemon is not already running, Emacs asks `zscheme daemon` to start and
then connects to its Unix socket.

## Trying it from ielm

Open an Elisp REPL:

```text
M-x ielm RET
```

Evaluate simple zscheme:

```elisp
(ma-eval "(+ 1 2)")
```

Read and write local zscheme config:

```elisp
(ma-set ".my.config.greeting" "hei")
(ma-get ".my.config.greeting")
```

Call an actor through zscheme:

```elisp
(ma-rpc "@sky#room" ":look")
```

The return value is a normal Elisp value, usually a string or nil. You can use
it directly in Elisp:

```elisp
(message "Room says: %s" (ma-rpc "@sky#room" ":look"))
```

## Runtime and room examples

Assume `@sky` is the runtime alias and `#concourse` is the room:

```elisp
(require 'ma)
(require 'ma-edit)
(ma-connect)

(setq ma-runtime "@sky")
(setq ma-room (concat ma-runtime "#concourse"))
```

Enter by calling the room directly. Lambda-ma rooms do not require ordinary
clients to send `kind=avatar`; send only ordinary identity fields and let the
world choose the effective kind. The committed context comes back asynchronously
as `:ctx` with protocol `/ma/lambda/ctx/0.0.1` and names the avatar actor to use
for user commands.

```elisp
(ma-eval
 "(rpc-send \"@sky#concourse\" \"enter\"
   (make-map \"name\" \"alice\"
          \"nick\" \"alice\"
          \"description\" \"A zscheme user.\"))")
```

After reading the returned lambda ctx, set `ma-avatar` to its `:avatar` value.
User commands go through that avatar:

```elisp
(setq ma-avatar "did:ma:...#avatar")
(ma-rpc ma-avatar ":look")
(ma-rpc ma-avatar ":say" "hello world")
(ma-rpc ma-avatar ":claim")
```

Room control methods go directly to the room:

```elisp
(ma-rpc ma-room ":prop" "name" "The Workshop")
(ma-rpc ma-room ":prop" "description"
        "A bright room with cables on the floor and a half-built door in the north wall.")
(ma-rpc ma-room ":look")
```

### Add one room method

Emacs can edit local zscheme paths directly. It does not yet have zion's
integrated `:behaviour!edit` publishing flow, so the current path is: edit
locally, publish the file to IPFS, then point the room at the returned
`/ipfs/<cid>`.

```elisp
(ma-edit-path ".my.doc.room-behaviour")
```

Put this in the edit buffer and save it with `C-c C-c`:

```scheme
(set-method! :hello-world
  (lambda (args msg)
    (ma-send! (msg-from msg)
      (list :print "hello world from this room"))))
```

Write the saved path to a normal file and publish it:

```elisp
(with-temp-file "/tmp/room-behaviour.ma"
  (insert (ma-get ".my.doc.room-behaviour")))
```

```sh
ipfs add --quieter /tmp/room-behaviour.ma
```

Set the room behaviour CID and call the new method:

```elisp
(ma-rpc ma-room ":behaviour" "/ipfs/bafy...returned-cid...")
(ma-rpc ma-room ":hello-world")
```

## Editing `.zcm` files

Create or open a file ending in `.zcm`:

```text
C-x C-f test.zcm RET
```

Emacs should show `ma` in the mode line. Useful keys in `ma-mode`:

- `C-c C-c` evaluates the whole buffer through the zscheme daemon.
- `C-c C-r` evaluates the active region.
- `C-c C-e` evaluates the expression before point.
- `C-c C-z` connects to the daemon explicitly.

Evaluation output appears in the `*ma output*` buffer.

## Editing a path without closing the editor

Open any local zscheme path in a normal Emacs buffer:

```elisp
(ma-edit-path ".my.doc.notes")
```

Emacs opens a buffer in a new frame. Edit the text normally. Then:

- `C-c C-c` saves the buffer back to the ma path and keeps the buffer/frame
  open.
- `C-c C-k` closes the edit buffer.

This is deliberately different from zion's modal editor flow: you can keep a
document open, edit, save, test, edit again, and save again without reopening
the editor every time.

## What works in this first slice

- Socket discovery matching the zscheme daemon.
- Auto-spawn of `zscheme daemon`.
- Hello handshake.
- Synchronous zscheme evaluation through `ma-eval`.
- Thin helpers: `ma-get`, `ma-set`, `ma-delete`, and `ma-rpc`.
- `ma-mode` for `.zcm` files with buffer/region/last-expression eval.
- Persistent edit buffers for local ma paths via `ma-edit-path`.

Focus routing and inbox view come next.
