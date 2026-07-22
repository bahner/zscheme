;;; ma-ipc.el --- zscheme daemon IPC for Emacs -*- lexical-binding: t; -*-

;; This file is intentionally dependency-free. It implements only the small
;; CBOR subset used by zscheme's serde/ciborium IPC protocol.

;;; Code:

(require 'cl-lib)
(require 'subr-x)

(defgroup ma-ipc nil
  "Talk to the zscheme backend daemon."
  :group 'applications)

(defcustom ma-ipc-program "zscheme"
  "Program used to auto-spawn the zscheme daemon."
  :type 'string
  :group 'ma-ipc)

(defcustom ma-ipc-connect-retries 40
  "Number of connection attempts after auto-spawning the daemon."
  :type 'integer
  :group 'ma-ipc)

(defcustom ma-ipc-connect-delay 0.25
  "Seconds between connection attempts after auto-spawning the daemon."
  :type 'number
  :group 'ma-ipc)

(defcustom ma-ipc-daemon-version "0.2.0"
  "Protocol version sent in the initial zscheme daemon hello request."
  :type 'string
  :group 'ma-ipc)

(defvar ma-ipc--socket nil)
(defvar ma-ipc--process nil)
(defvar ma-ipc--buffer nil)
(defvar ma-ipc--frames nil)
(defvar ma-ipc--next-id 1)

(defconst ma-ipc--max-frame-len (* 16 1024 1024))
(defun ma-ipc-socket-path ()
  "Return the zscheme daemon socket path for this user."
  (or (and (getenv "XDG_RUNTIME_DIR")
           (expand-file-name "zscheme.sock" (getenv "XDG_RUNTIME_DIR")))
      (let ((dir (expand-file-name "ma" (or (getenv "XDG_DATA_HOME")
                                             (expand-file-name ".local/share" "~")))))
        (make-directory dir t)
        (expand-file-name "zscheme.sock" dir))))

(defun ma-ipc-daemon-log-path ()
  "Return the zscheme daemon auto-spawn log path."
  (let ((dir (expand-file-name "ma" (or (getenv "XDG_DATA_HOME")
                                         (expand-file-name ".local/share" "~")))))
    (make-directory dir t)
    (expand-file-name "zscheme-daemon.log" dir)))

(defun ma-ipc--u8 (s i)
  (aref s i))

(defun ma-ipc--u16 (s i)
  (+ (ash (ma-ipc--u8 s i) 8)
     (ma-ipc--u8 s (1+ i))))

(defun ma-ipc--u32 (s i)
  (+ (ash (ma-ipc--u8 s i) 24)
     (ash (ma-ipc--u8 s (+ i 1)) 16)
     (ash (ma-ipc--u8 s (+ i 2)) 8)
     (ma-ipc--u8 s (+ i 3))))

(defun ma-ipc--read-uint (s i ai)
  (cond
   ((< ai 24) (cons ai i))
   ((= ai 24) (cons (ma-ipc--u8 s i) (1+ i)))
   ((= ai 25) (cons (ma-ipc--u16 s i) (+ i 2)))
   ((= ai 26) (cons (ma-ipc--u32 s i) (+ i 4)))
   ((= ai 27)
    (let ((hi (ma-ipc--u32 s i))
          (lo (ma-ipc--u32 s (+ i 4))))
      (cons (+ (* hi 4294967296) lo) (+ i 8))))
   (t (error "Unsupported CBOR additional information: %s" ai))))

(defun ma-ipc--encode-uint (major n)
  (cond
  ((< n 24) (unibyte-string (+ (ash major 5) n)))
  ((< n 256) (unibyte-string (+ (ash major 5) 24) n))
  ((< n 65536) (unibyte-string (+ (ash major 5) 25)
                      (logand (ash n -8) 255)
                                (logand n 255)))
  ((< n 4294967296) (unibyte-string (+ (ash major 5) 26)
                         (logand (ash n -24) 255)
                         (logand (ash n -16) 255)
                         (logand (ash n -8) 255)
                                     (logand n 255)))
  (t (unibyte-string (+ (ash major 5) 27)
               (logand (ash n -56) 255)
               (logand (ash n -48) 255)
               (logand (ash n -40) 255)
               (logand (ash n -32) 255)
               (logand (ash n -24) 255)
               (logand (ash n -16) 255)
               (logand (ash n -8) 255)
                      (logand n 255)))))

(defun ma-ipc--encode (value)
  (cond
   ((null value) (unibyte-string #xf6))
   ((eq value t) (unibyte-string #xf5))
   ((eq value :false) (unibyte-string #xf4))
   ((integerp value)
    (if (>= value 0)
        (ma-ipc--encode-uint 0 value)
      (ma-ipc--encode-uint 1 (- -1 value))))
   ((stringp value)
    (let ((bytes (encode-coding-string value 'utf-8 t)))
      (concat (ma-ipc--encode-uint 3 (length bytes)) bytes)))
   ((and (consp value) (eq (car value) :array))
    (concat (ma-ipc--encode-uint 4 (length (cdr value)))
            (mapconcat #'ma-ipc--encode (cdr value) "")))
   ((and (consp value) (eq (car value) :map))
    (let ((pairs (cdr value)))
      (concat (ma-ipc--encode-uint 5 (length pairs))
              (mapconcat (lambda (pair)
                           (concat (ma-ipc--encode (car pair))
                                   (ma-ipc--encode (cdr pair))))
                         pairs ""))))
   (t (error "Cannot CBOR-encode value: %S" value))))

(defun ma-ipc--decode (s &optional start)
  (let* ((i (or start 0))
         (head (ma-ipc--u8 s i))
         (major (ash head -5))
         (ai (logand head 31)))
    (setq i (1+ i))
    (pcase major
      (0 (ma-ipc--read-uint s i ai))
      (1 (let ((res (ma-ipc--read-uint s i ai)))
           (cons (- -1 (car res)) (cdr res))))
      (3 (pcase-let ((`(,len . ,next) (ma-ipc--read-uint s i ai)))
           (cons (decode-coding-string (substring s next (+ next len)) 'utf-8 t)
                 (+ next len))))
      (4 (pcase-let ((`(,len . ,next) (ma-ipc--read-uint s i ai)))
           (let ((items nil))
             (dotimes (_ len)
               (pcase-let ((`(,value . ,after) (ma-ipc--decode s next)))
                 (push value items)
                 (setq next after)))
             (cons (cons :array (nreverse items)) next))))
      (5 (pcase-let ((`(,len . ,next) (ma-ipc--read-uint s i ai)))
           (let ((pairs nil))
             (dotimes (_ len)
               (pcase-let* ((`(,key . ,after-key) (ma-ipc--decode s next))
                            (`(,value . ,after-value) (ma-ipc--decode s after-key)))
                 (push (cons key value) pairs)
                 (setq next after-value)))
             (cons (cons :map (nreverse pairs)) next))))
      (7 (cond
          ((= ai 20) (cons :false i))
          ((= ai 21) (cons t i))
          ((= ai 22) (cons nil i))
          (t (error "Unsupported CBOR simple value: %s" ai))))
      (_ (error "Unsupported CBOR major type: %s" major)))))

(defun ma-ipc--variant (name &optional value)
  (cons :map (list (cons name value))))

(defun ma-ipc--fields (&rest pairs)
  (cons :map pairs))

(defun ma-ipc--request (request)
  (pcase request
    (`(hello . ,version)
     (ma-ipc--variant "Hello" (ma-ipc--fields (cons "version" version))))
    (`(eval ,id ,source ,isolated)
     (ma-ipc--variant "Eval" (ma-ipc--fields (cons "id" id)
                                             (cons "source" source)
                                             (cons "isolated" (if isolated t :false)))))
    ('ping (ma-ipc--variant "Ping" nil))
    ('stop (ma-ipc--variant "Stop" nil))
    ('reset (ma-ipc--variant "Reset" nil))
    ('dump-env (ma-ipc--variant "DumpEnv" nil))
    (_ (error "Unknown zscheme IPC request: %S" request))))

(defun ma-ipc--alist-get (key alist)
  (cdr (assoc key alist)))

(defun ma-ipc--map-alist (value)
  (unless (and (consp value) (eq (car value) :map))
    (error "Expected CBOR map, got: %S" value))
  (cdr value))

(defun ma-ipc--unwrap-option (value)
  (cond
   ((null value) nil)
   ((and (consp value) (eq (car value) :map))
    (let ((variant (car (cdr value))))
      (pcase (car variant)
        ("Some" (cdr variant))
        ("None" nil)
        (_ value))))
   (t value)))

(defun ma-ipc--unwrap-result (value)
  (let* ((variant (car (ma-ipc--map-alist value)))
         (name (car variant))
         (payload (cdr variant)))
    (pcase name
      ("Ok" (list :ok (ma-ipc--unwrap-option payload)))
      ("Err" (list :error payload))
      (_ (error "Unknown Result variant: %S" name)))))

(defun ma-ipc--response (value)
  (let* ((variant (car (ma-ipc--map-alist value)))
         (name (car variant))
         (payload (cdr variant)))
    (pcase name
      ("HelloAck"
       (let ((fields (ma-ipc--map-alist payload)))
         (list 'hello-ack
               :version (ma-ipc--alist-get "version" fields)
               :did (ma-ipc--alist-get "did" fields))))
      ("Display"
       (let ((fields (ma-ipc--map-alist payload)))
         (list 'display
               :id (ma-ipc--alist-get "id" fields)
               :text (ma-ipc--alist-get "text" fields))))
      ("EvalResult"
       (let ((fields (ma-ipc--map-alist payload)))
         (list 'eval-result
               :id (ma-ipc--alist-get "id" fields)
               :outcome (ma-ipc--unwrap-result (ma-ipc--alist-get "outcome" fields)))))
      ("Pong" (list 'pong))
      ("Stopping" (list 'stopping))
      ("ResetDone" (list 'reset-done))
      ("EnvDump"
       (let ((fields (ma-ipc--map-alist payload)))
         (list 'env-dump :source (ma-ipc--alist-get "source" fields))))
      (_ (error "Unknown zscheme IPC response: %S" name)))))

(defun ma-ipc--frame (bytes)
  (let ((len (length bytes)))
    (when (> len ma-ipc--max-frame-len)
      (error "zscheme IPC frame too large: %s" len))
    (concat (unibyte-string (logand (ash len -24) 255)
                            (logand (ash len -16) 255)
                            (logand (ash len -8) 255)
                            (logand len 255))
            bytes)))

(defun ma-ipc--process-filter (_proc chunk)
  (setq chunk (encode-coding-string chunk 'binary t))
  (setq ma-ipc--buffer (concat ma-ipc--buffer chunk))
  (while (and (>= (length ma-ipc--buffer) 4)
              (let ((len (ma-ipc--u32 ma-ipc--buffer 0)))
                (when (> len ma-ipc--max-frame-len)
                  (error "zscheme IPC frame too large: %s" len))
                (>= (length ma-ipc--buffer) (+ 4 len))))
    (let* ((len (ma-ipc--u32 ma-ipc--buffer 0))
           (body (substring ma-ipc--buffer 4 (+ 4 len))))
      (setq ma-ipc--buffer (substring ma-ipc--buffer (+ 4 len)))
      (push (ma-ipc--response (car (ma-ipc--decode body))) ma-ipc--frames))))

(defun ma-ipc--sentinel (_proc event)
  (unless (string-match-p "finished\\|deleted" event)
    (message "zscheme daemon IPC: %s" (string-trim event))))

(defun ma-ipc--spawn-daemon ()
  (let ((log (ma-ipc-daemon-log-path)))
    (call-process shell-file-name nil nil nil shell-command-switch
                  (format "nohup %s daemon >> %s 2>&1 < /dev/null &"
                          (shell-quote-argument ma-ipc-program)
                          (shell-quote-argument log)))
    log))

(defun ma-ipc--open-process ()
  (setq ma-ipc--buffer ""
        ma-ipc--frames nil
        ma-ipc--process
        (make-network-process :name "zscheme-ipc"
                              :family 'local
                              :service ma-ipc--socket
                              :coding 'binary
                              :filter #'ma-ipc--process-filter
                              :sentinel #'ma-ipc--sentinel
                              :noquery t)))

(defun ma-ipc-connect (&optional no-spawn)
  "Connect to the zscheme daemon and perform the hello handshake.
When NO-SPAWN is non-nil, signal instead of auto-spawning a daemon."
  (interactive)
  (unless (process-live-p ma-ipc--process)
    (setq ma-ipc--socket (ma-ipc-socket-path))
    (let ((connected (condition-case nil
                         (progn (ma-ipc--open-process) t)
                       (file-error nil)))
          (attempts 0))
      (when (and (not connected) no-spawn)
        (error "No zscheme daemon listening at %s" ma-ipc--socket))
      (unless connected
        (ma-ipc--spawn-daemon))
      (while (and (not connected) (< attempts ma-ipc-connect-retries))
        (setq attempts (1+ attempts))
        (condition-case nil
            (progn (ma-ipc--open-process)
                   (setq connected t))
          (file-error
           (sleep-for ma-ipc-connect-delay))))
      (unless connected
        (error "zscheme daemon failed to start; see %s" (ma-ipc-daemon-log-path)))))
  (let ((reply (ma-ipc-request `(hello . ,ma-ipc-daemon-version))))
    (unless (eq (car reply) 'hello-ack)
      (error "Unexpected zscheme hello response: %S" reply))
    reply))

(defun ma-ipc-disconnect ()
  "Close the current zscheme daemon IPC connection."
  (interactive)
  (when (process-live-p ma-ipc--process)
    (delete-process ma-ipc--process))
  (setq ma-ipc--process nil
        ma-ipc--buffer nil
        ma-ipc--frames nil))

(defun ma-ipc--pop-response (predicate &optional timeout)
  (let ((deadline (+ (float-time) (or timeout 30)))
        found)
    (while (and (not found) (< (float-time) deadline))
      (setq ma-ipc--frames (nreverse ma-ipc--frames))
      (let ((remaining nil))
        (dolist (frame ma-ipc--frames)
          (if (and (not found) (funcall predicate frame))
              (setq found frame)
            (push frame remaining)))
        (setq ma-ipc--frames remaining))
      (unless found
        (accept-process-output ma-ipc--process 0.1)))
    (or found (error "Timed out waiting for zscheme IPC response"))))

(defun ma-ipc-request (request &optional predicate timeout)
  "Send REQUEST to the zscheme daemon and return the matching response.
PREDICATE selects the response frame to return. TIMEOUT is in seconds."
  (unless (process-live-p ma-ipc--process)
    (ma-ipc-connect))
  (process-send-string ma-ipc--process
                       (ma-ipc--frame (ma-ipc--encode (ma-ipc--request request))))
  (ma-ipc--pop-response (or predicate (lambda (_frame) t)) timeout))

(defun ma-ipc-eval (source &optional display-callback isolated timeout)
  "Evaluate zscheme SOURCE in the daemon and return nil or a string.
DISPLAY-CALLBACK receives any streamed `(display ...)` text. When ISOLATED is
non-nil, evaluate in a per-connection environment. TIMEOUT is in seconds."
  (let ((id ma-ipc--next-id)
        result)
    (setq ma-ipc--next-id (1+ ma-ipc--next-id))
    (ma-ipc-request `(eval ,id ,source ,isolated)
                    (lambda (frame)
                      (pcase (car frame)
                        ('display
                         (when (= (plist-get (cdr frame) :id) id)
                           (when display-callback
                             (funcall display-callback (plist-get (cdr frame) :text))))
                         nil)
                        ('eval-result
                         (when (= (plist-get (cdr frame) :id) id)
                           (setq result (plist-get (cdr frame) :outcome))
                           t))
                        (_ nil)))
                    timeout)
    (pcase (car result)
      (:ok (cadr result))
      (:error (error "%s" (cadr result)))
      (_ (error "Unexpected zscheme eval result: %S" result)))))

(provide 'ma-ipc)

;;; ma-ipc.el ends here