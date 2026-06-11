# Mora language server for Neovim

[LSP 集成] Mora 脚本语言的 Neovim 配置。

## 安装

`mora-lsp` 在 `$PATH` 上（或通过 `cmd` 参数指定绝对路径）。

把 `lua/mora-lsp.lua` 复制到你的 Neovim 配置目录：

```bash
cp lua/mora-lsp.lua ~/.config/nvim/lua/
```

## init.lua 配置

```lua
-- 启用 LSP
require('mora-lsp').setup({
    -- 可选：自定义命令
    -- cmd = { '/opt/mora/bin/mora-lsp' },

    -- 可选：自定义 capabilities（如果你用了 completion-nvim 等）
    -- capabilities = require('cmp_nvim_lsp').default_capabilities(),

    -- 可选：自定义 on_attach（设置 keymap）
    on_attach = function(client, bufnr)
        local bufopts = { noremap = true, silent = true, buffer = bufnr }
        vim.keymap.set('n', 'gd', vim.lsp.buf.definition, bufopts)
        vim.keymap.set('n', 'gr', vim.lsp.buf.references, bufopts)
        vim.keymap.set('n', 'K', vim.lsp.buf.hover, bufopts)
        vim.keymap.set('n', '<C-Space>', vim.lsp.buf.completion, bufopts)
        vim.keymap.set('n', '<leader>rn', vim.lsp.buf.rename, bufopts)
        vim.keymap.set('n', '<leader>f', function()
            vim.lsp.buf.format({ async = true })
        end, bufopts)
    end,
})
```

## filetype 检测

如果你的 Neovim 没有自动识别 `.mora` 文件，可以加：

```lua
vim.api.nvim_create_autocmd('BufRead,BufNewFile', {
    pattern = '*.mora',
    callback = function() vim.bo.filetype = 'mora' end,
})
```

## Treesitter 高亮

`mora` 还不在 treesitter 官方 grammar 列表，可以临时用 Lua 的 grammar 或 syntax 文件做高亮。本仓库的 `editors/vscode/syntaxes/mora.tmLanguage.json` 是 TextMate 格式，Neovim 用 `vim-tmux-syntax` 或 `nvim-treesitter` 兼容层可以用。

## 旧版 Neovim (0.10 之前)

`vim.lsp.start` 是 0.11 才加的。0.10 用 `vim.lsp.start_client`：

```lua
vim.api.nvim_create_autocmd('FileType', {
    pattern = 'mora',
    callback = function(args)
        vim.lsp.start_client({
            name = 'mora',
            cmd = { 'mora-lsp' },
        }, vim.fn.bufnr(args.file))
    end,
})
```
