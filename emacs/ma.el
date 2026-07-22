;;; ma.el --- Thin Emacs API for the ma actor network -*- lexical-binding: t; -*-

;;; Code:

(require 'subr-x)
(require 'ma-ipc)

(defgroup ma nil
  "Emacs helpers for the ma actor network via zscheme."
  :group 'applications)

(defun ma--quote-zscheme-string (value)
  (format "%S" value))

(defun ma--dot-source (path)
  (string-trim-left path "\\."))

(defun ma-eval (source &optional display-callback isolated timeout)
  "Evaluate zscheme SOURCE through the zscheme daemon.
DISPLAY-CALLBACK receives streamed display output. ISOLATED asks the daemon for
a per-connection environment. TIMEOUT is in seconds."
  (ma-ipc-eval source display-callback isolated timeout))

(defun ma-get (path)
  "Read local zscheme config PATH and return its value."
  (ma-eval (format "(.%s)" (ma--dot-source path))))

(defun ma-set (path value)
  "Set local zscheme config PATH to VALUE."
  (ma-eval (format "(.%s: %s)" (ma--dot-source path) (ma--quote-zscheme-string value))))

(defun ma-delete (path)
  "Delete local zscheme config PATH or subtree."
  (ma-eval (format "(.%s:)" (ma--dot-source path))))

(defun ma-rpc (target verb &rest args)
  "Call TARGET with RPC VERB and ARGS via zscheme."
  (ma-eval
   (format "(rpc-send %s %s%s)"
           (ma--quote-zscheme-string target)
           (ma--quote-zscheme-string verb)
           (if args
               (concat " " (mapconcat #'ma--quote-zscheme-string args " "))
             ""))))

(defun ma-connect ()
  "Connect to the zscheme daemon, auto-spawning it if needed."
  (interactive)
  (let ((reply (ma-ipc-connect)))
    (message "Connected to zscheme daemon as %s" (plist-get (cdr reply) :did))))

(defun ma-disconnect ()
  "Close the current zscheme daemon connection."
  (interactive)
  (ma-ipc-disconnect)
  (message "Disconnected from zscheme daemon"))

(provide 'ma)

;;; ma.el ends here