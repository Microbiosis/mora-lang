# Editor Integrations

Mora  LSP[mora-lsp](../README.md#lsp-)****`mora-lsp`  `$PATH` 

|  |  |  | LSP  |
|--------|----------|------|------------|
| [VS Code](./vscode/) | `editors/vscode/` |  VSIX |  |
| [Neovim](./neovim/) | `editors/neovim/` | require  |  |
| [Helix](./helix/) | `editors/helix/` |  TOML |  |
| [Sublime Text](./sublime/) | `editors/sublime/` |  |  |
| [Vim](./vim/) | `editors/vim/` | ftplugin  |  vim-lsp/lspci |
| [Emacs](./emacs/) | `editors/emacs/` | load-path  |  lsp-mode |

##  mora-lsp 

`mora-lsp`  GitHub Releases CI  `chmod +x` +  `$PATH`

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

##  LSP 

 `examples/file_io.mora`

- 
- Hover `file.read_text`  `string`
-  `file.`  completion (read_text / write_text / ...)
- `F12`  `file.read_text` 

## 

 CLI  +  80% 

```bash
cargo run --release -- examples/file_io.mora   # 
MORA_NO_TYPECK=0 cargo run --release -- examples/file_io.mora  #  typeck
```
