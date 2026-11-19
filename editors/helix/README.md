# Helix editor

[LSP 集成] Mora 在 Helix 中通过 `languages.toml` 配置 `mora-lsp`。

## 配置

把 `languages.toml` 内容追加到 `~/.config/helix/languages.toml`，或直接拷贝覆盖。

确保 `mora-lsp` 在 `$PATH`：

```bash
# 安装预编译二进制（内含 mora 与 mora-lsp）
curl -L https://github.com/Microbiosis/mora-lang/releases/latest/download/mora-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv mora mora-lsp /usr/local/bin/

# 启动 helix，编辑 .mora 文件时自动启用 LSP
hx examples/hm_basic_demo.mora
```

## 验证 LSP 启动

在 Helix 正常模式输入 `:lsp-workspace-command status`，应该看到 `mora` 客户端处于 active 状态。

## 快捷键

| 键位 | 动作 |
|------|------|
| `K` | hover |
| `gd` | go-to-definition |
| `gr` | find-references |
| `<C-Space>` | completion |
| `<leader>rn` | rename |
| `<leader>f` | format |

## 语法高亮

Mora 暂无现成的 tree-sitter grammar。Helix 会使用 `mora-lsp` 提供的 LSP semantic tokens 与基础高亮。如果你有 tree-sitter 经验，欢迎贡献 `grammars/mora.wasm`。
