# Mora for VS Code

[Mora](https://github.com/Microbiosis/mora-lang) language support: syntax highlighting + LSP-backed diagnostics/hover/completion.

## Install

### From Marketplace (待发布)
1. Open VS Code
2. Extensions → search "Mora" → Install

### From local VSIX
```bash
# 在本目录构建
cd editors/vscode
npm install -g @vscode/vsce
vsce package  # 生成 mora-0.1.0.vsix

# 安装到 VS Code
code --install-extension mora-0.1.0.vsix
```

### From source
```bash
cd editors/vscode
npm install
npm run compile
# 启动调试：F5 in VS Code
```

## Prerequisite

`mora-lsp` binary on `$PATH` (or set `mora.languageServer.path`):

```json
{
  "mora.languageServer.path": "/path/to/mora-lsp"
}
```

Pre-built binaries: see [Releases](https://github.com/Microbiosis/mora-lang/releases).

## Features

- Syntax highlighting (keywords, strings, numbers, types, builtins)
- Inline error squiggles (from typeck)
- Hover on identifiers: shows type signature
- Go-to-definition (F12)
- Find references (Shift+F12)
- Autocompletion (Ctrl+Space): keywords, variables, tasks, builtin modules
- Format document (Shift+Alt+F)
- Rename symbol (F2)
- Outline (Ctrl+Shift+O)

## Disable type checking

```json
{
  "mora.noTypeCheck": true
}
```

## Build

```bash
tsc -p .
```
