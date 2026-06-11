" Vim 8+ with :packadd or any vim-plug/vundle setup
" 把本文件作为 ~/.vim/ftplugin/mora.vim 安装

" Filetype detection
autocmd BufRead,BufNewFile *.mora set filetype=mora

" Comments: '--'
setlocal commentstring=--\ %s
setlocal comments=:--\:

" Indent: 2 空格
setlocal tabstop=2
setlocal shiftwidth=2
setlocal softtabstop=2
setlocal expandtab

" 简易语法高亮（vim 8+）
if has('syntax')
    syntax clear
    syntax case ignore
    syntax keyword moraKeyword let task fn if then end for in while do try catch return parallel match with import export save load read write append true false nil
    syntax keyword moraType string number bool list dict task closure
    syntax match moraComment "\-\-.*$"
    syntax region moraString start='"' end='"'
    syntax match moraNumber "\<\d\+\(\.\d\+\)\?\>"
    syntax match moraBuiltin "^\s*\.\zs\w\+" contained
    highlight default link moraKeyword Keyword
    highlight default link moraType Type
    highlight default link moraComment Comment
    highlight default link moraString String
    highlight default link moraNumber Number
    highlight default link moraBuiltin Function
endif

" --- LSP integration (requires vim-lsp or vim-lsc) ---
" 例子：vim-lsp
if exists('*lsp#enable')
    au User lsp_setup call lsp#register_server({
        \ 'name': 'mora',
        \ 'cmd': {server_info->['mora-lsp']},
        \ 'allowlist': ['mora'],
    \ })
    autocmd FileType mora call lsp#enable()
endif
