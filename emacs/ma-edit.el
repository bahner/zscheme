;;; ma-edit.el --- Edit ma paths in persistent Emacs buffers -*- lexical-binding: t; -*-

;;; Code:

(require 'ma)

(defvar-local ma-edit-path nil)

(defvar ma-edit-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-c") #'ma-edit-save)
    (define-key map (kbd "C-c C-k") #'ma-edit-close)
    map)
  "Keymap for `ma-edit-mode'.")

(define-derived-mode ma-edit-mode text-mode "ma-edit"
  "Major mode for editing one ma config path.")

(defun ma-edit-buffer-name (path)
  "Return the editor buffer name for PATH."
  (format "*ma edit %s*" path))

(defun ma-edit-path (path)
  "Open PATH in a persistent Emacs buffer.
Save with `C-c C-c`. Close with `C-c C-k`. Saving does not kill the buffer or
close the frame."
  (interactive "sma path: ")
  (let* ((value (or (ma-get path) ""))
         (buffer (get-buffer-create (ma-edit-buffer-name path))))
    (with-current-buffer buffer
      (let ((inhibit-read-only t))
        (erase-buffer)
        (insert value)
        (ma-edit-mode)
        (setq ma-edit-path path)
        (set-buffer-modified-p nil)))
    (display-buffer-pop-up-frame buffer nil)
    buffer))

(defun ma-edit-save ()
  "Save the current `ma-edit-mode' buffer back to its ma path."
  (interactive)
  (unless ma-edit-path
    (error "This buffer is not attached to a ma path"))
  (ma-set ma-edit-path (buffer-substring-no-properties (point-min) (point-max)))
  (set-buffer-modified-p nil)
  (message "Saved %s" ma-edit-path))

(defun ma-edit-close ()
  "Kill the current ma edit buffer."
  (interactive)
  (kill-buffer (current-buffer)))

(provide 'ma-edit)

;;; ma-edit.el ends here