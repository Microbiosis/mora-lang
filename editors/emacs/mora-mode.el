;;; mora-mode.el --- Major mode for Mora  -*- lexical-binding: t -*-
;;;
;;; Usage:
;;;   (require 'mora-mode)
;;;   (add-to-list 'auto-mode-alist '("\\.mora\\'" . mora-mode))
;;;
;;; 简易实现：缩进 + 注释 + 关键词高亮（v0.110.0）。LSP 集成依赖 lsp-mode。

;;; Code:

(defgroup mora nil "Mora language support." :group 'languages)

(defcustom mora-lsp-server-executable "mora-lsp"
  "Path to the mora-lsp binary."
  :type 'string
  :group 'mora)

(defvar mora-mode-syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?\" "\"" table)   ; string
    (modify-syntax-entry ?\n ">   " table)  ; newline ends comment
    (modify-syntax-entry ?- ". 12" table)  ; - is punctuation, -- is comment
    table)
  "Syntax table for Mora mode.")

(defvar mora-font-lock-keywords
  (list
   '("\\b\\(let\\|assign\\|task\\|fn\\|if\\|then\\|else\\|end\\|for\\|in\\|while\\|loop\\|break\\|continue\\|do\\|return\\|parallel\\|worker\\|transaction\\|compensation\\|commit\\|rollback\\|match\\|with\\|when\\|import\\|export\\|struct\\|enum\\|trait\\|impl\\|dyn\\|as\\|type\\|macro\\|effect\\|perform\\|handle\\|spawn\\|select\\|case\\|timeout\\|default\\|no_grad\\|grad\\|value_and_grad\\|jvp\\|trace_dag\\|quote\\|eval\\|read\\|into\\|and\\|or\\|not\\|true\\|false\\|nil\\|Self\\)\\b"
     . font-lock-keyword-face)
   '("\\b\\(string\\|char\\|int\\|float\\|number\\|bool\\|nil\\|f16\\|bf16\\|fp8\\|fp8_e4m3\\|fp8_e5m2\\|f32\\|f64\\|i4\\|i8\\|i16\\|i32\\|i64\\|u8\\|u16\\|u32\\|u64\\|list\\|dict\\|tensor\\|Tensor\\|compute_dag\\|kv_allocator\\|memory_pool\\|task\\|closure\\|result\\|any\\|channel\\|task_handle\\|context\\|tcp_listener\\|tcp_stream\\|continuation\\|external\\|atom\\)\\b"
     . font-lock-type-face)
   '("\\b\\(json\\|file\\|exec\\|math\\|stats\\|linalg\\|random\\|Tensor\\)\\b" . font-lock-builtin-face)
   '("^\\s-*\\(--.*\\)$" . font-lock-comment-face)
   '("\\<\\([0-9]+\\(?:\\.[0-9]+\\)?\\)\\>" . font-lock-constant-face)))

;;;###autoload
(define-derived-mode mora-mode prog-mode "Mora"
  "Major mode for editing Mora scripts."
  :syntax-table mora-mode-syntax-table
  (setq font-lock-defaults '(mora-font-lock-keywords))
  (setq-local comment-start "-- ")
  (setq-local comment-end "")
  (setq-local tab-width 2)
  (setq-local indent-tabs-mode nil))

;;;###autoload
(add-to-list 'auto-mode-alist '("\\.mora\\'" . mora-mode))

(provide 'mora-mode)
;;; mora-mode.el ends here
