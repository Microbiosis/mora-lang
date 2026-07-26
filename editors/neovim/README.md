# Mora language server for Neovim

[LSP ] Mora  Neovim 

## 

`mora-lsp`  `$PATH`  `cmd` 

 `lua/mora-lsp.lua`  Neovim 

```bash
cp lua/mora-lsp.lua ~/.config/nvim/lua/
```

## init.lua 

```lua
--  LSP
require('mora-lsp').setup({
    -- 
    -- cmd = { '/opt/mora/bin/mora-lsp' },

    --  capabilities completion-nvim 
    -- capabilities = require('cmp_nvim_lsp').default_capabilities(),

    --  on_attach keymap
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

## filetype 

 Neovim  `.mora` 

```lua
vim.api.nvim_create_autocmd('BufRead,BufNewFile', {
    pattern = '*.mora',
    callback = function() vim.bo.filetype = 'mora' end,
})
```

## Treesitter 

`mora`  treesitter  grammar  Lua  grammar  syntax  `editors/vscode/syntaxes/mora.tmLanguage.json`  TextMate Neovim  `vim-tmux-syntax`  `nvim-treesitter` 

##  Neovim (0.10 )

`vim.lsp.start`  0.11 0.10  `vim.lsp.start_client`

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
