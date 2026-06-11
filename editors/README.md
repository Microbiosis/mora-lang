# Editor Integrations

Mora 通过 LSP（[mora-lsp](../README.md#lsp-语言服务)）支持主流编辑器。**前置条件**：`mora-lsp` 在 `$PATH` 上，或在配置中指定绝对路径。

| 编辑器 | 配置目录 | 难度 | LSP 完整度 |
|--------|----------|------|------------|
| [VS Code](./vscode/) | `editors/vscode/` | 中（需编译 VSIX） | 完整 |
| [Neovim](./neovim/) | `editors/neovim/` | 低（require 一行） | 完整 |
| [Helix](./helix/) | `editors/helix/` | 低（追加 TOML） | 完整 |
| [Sublime Text](./sublime/) | `editors/sublime/` | 低（单文件） | 完整 |
| [Vim](./vim/) | `editors/vim/` | 低（ftplugin 即可） | 完整（需 vim-lsp/lspci） |
| [Emacs](./emacs/) | `editors/emacs/` | 低（load-path 一行） | 完整（需 lsp-mode） |

## 安装预编译 mora-lsp 二进制

`mora-lsp` 通过 GitHub Releases 发布（CI 自动多平台构建）。下载后 `chmod +x` + 放入 `$PATH`：

```bash
# Linux x86_64
curl -L -o mora-lsp https://github.com/Microbiosis/mora-lang/releases/latest/download/mora-lsp-x86_64-unknown-linux-gnu
chmod +x mora-lsp
sudo mv mora-lsp /usr/local/bin/

# macOS Apple Silicon
curl -L -o mora-lsp https://github.com/Microbiosis/mora-lang/releases/latest/download/mora-lsp-aarch64-apple-darwin
chmod +x mora-lsp && sudo mv mora-lsp /usr/local/bin/

# Windows (PowerShell)
Invoke-WebRequest -Uri "https://github.com/Microbiosis/mora-lang/releases/latest/download/mora-lsp-x86_64-pc-windows-msvc.exe" -OutFile "mora-lsp.exe"
Move-Item mora-lsp.exe "$env:USERPROFILE\bin\"
```

## 验证 LSP 启动

任一编辑器中打开 `examples/file_io.mora`：

- 类型错误的行有红色波浪线
- Hover `file.read_text` 显示返回类型 `string`
- 输入 `file.` 看到 completion (read_text / write_text / ...)
- `F12` 在 `file.read_text` 上跳到定义（如果有的话）

## 不做编辑器配置？

直接用 CLI 解释器 + 类型检查也能获得 80% 体验：

```bash
cargo run --release -- examples/file_io.mora   # 解释执行
MORA_NO_TYPECK=0 cargo run --release -- examples/file_io.mora  # 启用 typeck
```
