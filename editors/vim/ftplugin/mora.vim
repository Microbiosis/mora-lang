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

" 简易语法高亮（vim 8+，v0.110.0）
if has('syntax')
    syntax clear
    syntax case ignore
    syntax keyword moraKeyword let assign task fn if then else end for in while loop break continue do return parallel worker transaction compensation commit rollback match with when import export struct enum trait impl dyn as type macro effect perform handle spawn select case timeout default no_grad grad value_and_grad jvp trace_dag quote eval read into and or not true false nil Self
    syntax keyword moraType string char int float number bool nil f16 bf16 fp8 fp8_e4m3 fp8_e5m2 f32 f64 i4 i8 i16 i32 i64 u8 u16 u32 u64 list dict tensor Tensor compute_dag kv_allocator memory_pool task closure result any channel task_handle context tcp_listener tcp_stream continuation external atom
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
