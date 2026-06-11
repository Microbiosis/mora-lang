-- Mora language server for Neovim >= 0.11
--
-- 用法 1（推荐）：在 init.lua 中 require 本文件
--   local mora_lsp = require('mora-lsp')
--   mora_lsp.setup({})
--
-- 用法 2：直接 inline 复制下面的 setup 到你的 init.lua

local M = {}

---@class MoraLspOpts
---@field cmd string[]|nil Override the mora-lsp command (default: {"mora-lsp"})
---@field capabilities table|nil LSP capabilities
---@field on_attach fun(client, bufnr)|nil Custom on_attach

---Setup Mora LSP for current Neovim
---@param opts MoraLspOpts|nil
function M.setup(opts)
    opts = opts or {}
    local cmd = opts.cmd or { 'mora-lsp' }

    vim.api.nvim_create_autocmd('FileType', {
        pattern = 'mora',
        callback = function(args)
            local client_id = vim.lsp.start({
                name = 'mora',
                cmd = cmd,
                filetypes = { 'mora' },
                capabilities = opts.capabilities or vim.lsp.protocol.make_client_capabilities(),
                on_attach = opts.on_attach,
                root_dir = vim.fs.dirname(vim.fs.find('.mora.toml', { upward = true })[1] or vim.fn.getcwd()),
            })
            if not client_id then
                vim.notify('mora-lsp: failed to start (is mora-lsp on $PATH?)', vim.log.levels.WARN)
            end
        end,
    })
end

-- Mora 文件类型检测（如果没装 filetype plugin）
vim.api.nvim_create_autocmd('BufRead,BufNewFile', {
    pattern = '*.mora',
    callback = function()
        vim.bo.filetype = 'mora'
    end,
})

return M
