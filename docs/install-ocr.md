# Mora OCR 模型安装 (v0.29+)

v0.28 的 `.rten` 模型从仓库 vendored 改为 user-side 文件 (~12 MB 减小)。
首次 OCR 调用前, 请执行以下步骤之一:

## 方式 1: 自动下载 (推荐)

```bash
mkdir -p ~/.local/share/mora/ocr
curl -O https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten
curl -O https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten
sha256sum text-detection.rten
sha256sum text-recognition.rten
mv text-*.rten ~/.local/share/mora/ocr/
```

验证 SHA256 与 `.git/sdd/ocrs-shasums.txt` 匹配。

## 方式 2: 包管理器

| 平台 | 包名 | 路径 |
|---|---|---|
| Debian/Ubuntu | `mora-ocr-models` | `/usr/share/mora/ocr/` (用 `MORA_OCR_MODELS_DIR=/usr/share/mora/ocr` 覆盖) |
| macOS Homebrew | `mora-ocr-models` | `/opt/homebrew/share/mora/ocr/` |
| Windows Scoop | `mora-ocr-models` | `%LOCALAPPDATA%\mora\ocr\` |

## 自定义路径

```bash
export MORA_OCR_MODELS_DIR=/your/path
mora
```

`MORA_OCR_MODELS_DIR` 优先级最高; 设置后将完全覆盖默认的
`$XDG_DATA_HOME/mora/ocr/` (或 `$HOME/.local/share/mora/ocr/`) 查找路径。

## 验证

```bash
mora  # REPL
> document.parse("./tests/fixtures/sample.png").text()
# 若返回非空字符串, 模型已就位
```

## 故障排除

若遇到 `ocr.load: model file '...' not found. Run 'mora-install-ocr' to download.`,
说明 Mora 找不到模型文件. 检查:

1. `~/.local/share/mora/ocr/` 是否存在且包含两个 `.rten` 文件
2. 是否有 `MORA_OCR_MODELS_DIR` 环境变量覆盖到不存在的目录
3. 验证 SHA256 与 `.git/sdd/ocrs-shasums.txt` 匹配
