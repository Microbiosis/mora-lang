;;; mora-mode.el --- Major mode for Mora  -*- lexical-binding: t -*-
;;;
;;; Usage:
;;;   (require 'mora-mode)
;;;   (add-to-list 'auto-mode-alist '("\\.mora\\'" . mora-mode))
;;;
;;; 简易实现：缩进 + 注释 + 关键词高亮。LSP 集成依赖 lsp-mode。

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
   '("\\b\\(let\\|task\\|fn\\|if\\|then\\|end\\|for\\|in\\|while\\|do\\|try\\|catch\\|return\\|parallel\\|match\\|with\\|import\\|export\\|save\\|load\\|read\\|write\\|append\\|true\\|false\\|nil\\|read_bytes\\|write_bytes\\|into\\)\\b"
     . font-lock-keyword-face)
   '("\\b\\(string\\|number\\|bool\\|list\\|dict\\|task\\|closure\\|conversation\\)\\b"
     . font-lock-type-face)
   '("\\b\\(ai\\|web\\|json\\|file\\)\\b" . font-lock-builtin-face)
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
