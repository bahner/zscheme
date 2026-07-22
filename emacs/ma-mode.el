;;; ma-mode.el --- Major mode for zscheme ma files -*- lexical-binding: t; -*-

;;; Code:

(require 'scheme)
(require 'ma)

(defgroup ma-mode nil
  "Major mode for zscheme files used with the ma actor network."
  :group 'ma)

(defcustom ma-mode-file-regexp "\\.zcm\\'"
  "File-name regexp used for `ma-mode'."
  :type 'regexp
  :group 'ma-mode)

(defvar ma-mode-output-buffer "*ma output*"
  "Buffer used for zscheme evaluation output from `ma-mode'.")

(defvar ma-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map scheme-mode-map)
    (define-key map (kbd "C-c C-c") #'ma-mode-eval-buffer)
    (define-key map (kbd "C-c C-r") #'ma-mode-eval-region)
    (define-key map (kbd "C-c C-e") #'ma-mode-eval-last-sexp)
    (define-key map (kbd "C-c C-z") #'ma-connect)
    map)
  "Keymap for `ma-mode'.")

(defconst ma-mode-font-lock-keywords
  (append
   scheme-font-lock-keywords-2
   `((,(regexp-opt '("define" "lambda" "let" "let*" "letrec" "if" "cond"
                     "begin" "and" "or" "when" "unless" "set!" "quote"
                     "guard" "include" "rpc-send" "ok?" "ok-val" "err?"
                     "err-msg") 'symbols)
      . font-lock-keyword-face)
     ("\\_<[@.][^][() \t\n]+" . font-lock-constant-face)
     ("\\_<#[^][() \t\n]+" . font-lock-string-face))))

;;;###autoload
(define-derived-mode ma-mode scheme-mode "ma"
  "Major mode for zscheme files.

Key bindings:
\{ma-mode-map}"
  (setq-local font-lock-defaults '(ma-mode-font-lock-keywords))
  (setq-local comment-start ";")
  (setq-local comment-end ""))

;;;###autoload
(add-to-list 'auto-mode-alist (cons ma-mode-file-regexp #'ma-mode))

(defun ma-mode--append-output (text)
  (with-current-buffer (get-buffer-create ma-mode-output-buffer)
    (goto-char (point-max))
    (insert text)))

(defun ma-mode--show-result (source result)
  (with-current-buffer (get-buffer-create ma-mode-output-buffer)
    (goto-char (point-max))
    (unless (bolp)
      (insert "\n"))
    (insert ";; ma eval\n")
    (insert source)
    (unless (string-suffix-p "\n" source)
      (insert "\n"))
    (when result
      (insert "=> " result "\n")))
  (display-buffer ma-mode-output-buffer))

(defun ma-mode-eval-string (source)
  "Evaluate zscheme SOURCE and show the result."
  (let ((result (ma-eval source #'ma-mode--append-output)))
    (ma-mode--show-result source result)
    result))

(defun ma-mode-eval-buffer ()
  "Evaluate the current buffer as zscheme source."
  (interactive)
  (ma-mode-eval-string (buffer-substring-no-properties (point-min) (point-max))))

(defun ma-mode-eval-region (beg end)
  "Evaluate the active region as zscheme source."
  (interactive "r")
  (unless (use-region-p)
    (user-error "No active region"))
  (ma-mode-eval-string (buffer-substring-no-properties beg end)))

(defun ma-mode-eval-last-sexp ()
  "Evaluate the zscheme expression before point."
  (interactive)
  (let ((end (point))
        (beg (save-excursion
               (backward-sexp)
               (point))))
    (ma-mode-eval-string (buffer-substring-no-properties beg end))))

(provide 'ma-mode)

;;; ma-mode.el ends here