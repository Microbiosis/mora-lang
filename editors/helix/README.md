# Helix editor

[LSP ] Mora  Helix  `languages.toml`  LSP server

## 

 `languages.toml`  `~/.config/helix/languages.toml`

 `mora-lsp`  `$PATH`

```bash
# 
curl -L https://github.com/Microbiosis/mora-lang/releases/latest/download/mora-lsp-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv mora-lsp /usr/local/bin/

#  helix .mora  LSP
hx examples/embed_demo.mora
```

##  LSP 

 Helix  `:lsp-workspace-command status` `mora`  active

## 

|  |  |
|------|------|
| `K` | hover |
| `gd` | go-to-definition |
| `gr` | find-references |
| `<C-Space>` | completion |
| `<leader>rn` | rename |
| `<leader>f` | format |

## 

Mora  tree-sitter grammarHelix ——///

 tree-sitter  `grammars/mora.wasm`
