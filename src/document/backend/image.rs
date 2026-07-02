//! v0.29: ImageBackend — pure-Rust OCR via ocrs + image + user-side `.rten`
//!
//! Pipeline: `from_bytes` decodes the PNG (via the `image` crate) to populate
//! `width`/`height`, then runs OCR (via `ocrs`, backed by `rten` ONNX-style
//! `.rten` models) to populate `lines`. The OCR pipeline:
//!
//! 1. Decode the PNG into a `DynamicImage` and convert to RGB8.
//! 2. Build an `ocrs::ImageSource` from the RGB pixel buffer.
//! 3. Run `OcrEngine::prepare_input` (greyscale + normalize to [-0.5, 0.5]).
//! 4. Run `OcrEngine::get_text` (detect_words → find_text_lines → recognize).
//!
//! IMPORTANT — model files (v0.29 user-side migration):
//! ocrs does NOT bundle its ONNX-style `.rten` models inside the crate.
//! `OcrEngineParams { detection_model: None, recognition_model: None }` would
//! produce an engine that errors on the first `detect_words` / `get_text`
//! call with `"Detection model not loaded"`.
//!
//! In v0.28 we vendored both `.rten` files (12 MB total) under
//! `tests/fixtures/` and `include_bytes!`'d them at compile time. v0.29
//! moves to a user-side model directory (see `user_model_path` below):
//!
//! - Default: `$XDG_DATA_HOME/mora/ocr/` (Linux/macOS) or
//!   `$HOME/.local/share/mora/ocr/` (POSIX fallback when XDG_DATA_HOME is
//!   unset). On Windows we resolve via `%LOCALAPPDATA%\mora\ocr\`.
//! - Override: `MORA_OCR_MODELS_DIR` env var.
//!
//! We call `rten::Model::load_file(path)` (no feature flag needed; the
//! `rten_format` default feature pulls in `rten-model-file`) to hand the
//! file bytes to `OcrEngineParams`. The engine is memoised in a
//! `OnceLock<OcrEngine>` so subsequent `from_bytes` calls (e.g. across
//! the test suite) reuse the same instance. The first init takes ~1–3s;
//! subsequent calls are essentially free.
//!
//! See `docs/install-ocr.md` for one-time install instructions and
//! `.git/sdd/ocrs-shasums.txt` for the expected SHA256 of each model file.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::OnceLock;

use image::io::Reader as ImageReader;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};

use crate::document::DocumentBackend;
use crate::value::Value;

/// v0.29: 解析 OCR 模型路径 — user-side dir.
///
/// 优先级:
///
/// 1. `MORA_OCR_MODELS_DIR` — **整体**作为 ocr dir (用户/打包者指定完整路径,
///    例如 `/usr/share/mora/ocr` 或 `%LOCALAPPDATA%\mora\ocr`).
/// 2. `$XDG_DATA_HOME/mora/ocr/` (Linux/macOS XDG 规范).
/// 3. `$HOME/.local/share/mora/ocr/` (XDG_DATA_HOME 未设时的 POSIX fallback).
/// 4. `%LOCALAPPDATA%\mora\ocr\` (Windows fallback, v0.31 新增).
///
/// 如果全部无法解析, 返回 `Err(String)` 让调用方 fail-loud.
pub fn user_model_path(name: &str) -> Result<PathBuf, String> {
    let dir: PathBuf = if let Ok(p) = std::env::var("MORA_OCR_MODELS_DIR") {
        // override: user points DIRECTLY at the ocr dir
        PathBuf::from(p)
    } else {
        // default: XDG-ish (POSIX) or LOCALAPPDATA (Windows) — append `mora/ocr` to data root
        let base: PathBuf = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|_| {
                std::env::var("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .or_else(|_| {
                // v0.31: Windows fallback — %LOCALAPPDATA%\mora\ocr
                std::env::var("LOCALAPPDATA").map(PathBuf::from)
            })
            .map_err(|_| {
                "ocr.load: cannot resolve OCR model directory. Set MORA_OCR_MODELS_DIR \
                 or HOME / XDG_DATA_HOME (POSIX) or LOCALAPPDATA (Windows) to a writable path."
                    .to_string()
            })?;
        base.join("mora").join("ocr")
    };
    Ok(dir.join(name))
}

/// v0.29: 检查模型文件存在性, 不存在则 fail-loud.
pub fn require_model_file(name: &str) -> Result<PathBuf, String> {
    let p = user_model_path(name)?;
    if !p.exists() {
        return Err(format!(
            "ocr.load: model file '{}' not found. Run 'mora-install-ocr' to download, \
             or set MORA_OCR_MODELS_DIR to override the directory.",
            p.display()
        ));
    }
    Ok(p)
}

#[derive(Debug)]
pub struct ImageBackend {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String,     // "png" only in v0.28
    pub lines: Vec<String>, // OCR output, one per detected text line
    pub ocr_engine: String, // "rten" (rten ONNX inference runtime)
}

/// v0.29: load the OCR engine from the user-side model directory.
///
/// ocrs's engine wraps two `rten::Model` instances (detection +
/// recognition). Loading them costs ~1–3s on a warm machine, so we cache
/// the engine in a process-wide OnceLock. OnceLock gives us cheap
/// "init exactly once" semantics without any external mutability
/// primitives.
///
/// The `anyhow::Result` is sticky: if init fails (missing/corrupt model
/// file), we don't keep retrying on each call and re-paying the load cost.
/// To retry, callers must restart the process.
static OCR_ENGINE: OnceLock<anyhow::Result<OcrEngine>> = OnceLock::new();

/// Build (or return the previously-built) OCR engine. The returned error
/// is wrapped in a `String` so callers can propagate it through the
/// `from_bytes` `Result` boundary without leaking ocrs's `anyhow::Error`.
fn ocr_engine_singleton() -> Result<&'static OcrEngine, String> {
    let res = OCR_ENGINE.get_or_init(|| -> anyhow::Result<OcrEngine> {
        // v0.29: load from user-side model dir (not vendored include_bytes!).
        // Model::load_file returns Model; we propagate anyhow errors.
        let detection_path = require_model_file("text-detection.rten")
            .map_err(|e| anyhow::anyhow!("ocr.load (detection): {}", e))?;
        let recognition_path = require_model_file("text-recognition.rten")
            .map_err(|e| anyhow::anyhow!("ocr.load (recognition): {}", e))?;
        let detection_model = rten::Model::load_file(detection_path)
            .map_err(|e| anyhow::anyhow!("ocr.load: failed to load detection model: {}", e))?;
        let recognition_model = rten::Model::load_file(recognition_path)
            .map_err(|e| anyhow::anyhow!("ocr.load: failed to load recognition model: {}", e))?;
        OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
    });
    match res {
        Ok(engine) => Ok(engine),
        Err(e) => Err(format!("document.parse: ocrs engine init error: {}", e)),
    }
}

impl ImageBackend {
    /// Decode a PNG byte slice and run OCR on it.
    ///
    /// Steps:
    /// 1. Decode via `image::ImageReader` (only `png` is accepted in v0.28).
    /// 2. Convert to RGB8 (`ocrs::ImageSource::from_bytes` requires 1/3/4 ch).
    /// 3. OCR: prepare_input → get_text.
    /// 4. Split the recognised text into lines.
    pub fn from_bytes(bytes: Vec<u8>, format: String) -> Result<Self, String> {
        if format != "png" {
            return Err(format!(
                "document.parse: image format '{}' not supported (v0.28: png only)",
                format
            ));
        }

        // 1. Decode the PNG to get a DynamicImage.
        let img = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| format!("document.parse: image format guess error: {}", e))?
            .decode()
            .map_err(|e| format!("document.parse: image decode error: {}", e))?;

        let width = img.width();
        let height = img.height();
        let rgb = img.into_rgb8();

        // 2. OCR via ocrs (rten ONNX inference runtime).
        let engine = ocr_engine_singleton()?;
        let img_source = ImageSource::from_bytes(rgb.as_raw(), (width, height))
            .map_err(|e| format!("document.parse: ocrs ImageSource error: {}", e))?;
        let ocr_input = engine
            .prepare_input(img_source)
            .map_err(|e| format!("document.parse: ocrs prepare_input error: {}", e))?;
        let text = engine
            .get_text(&ocr_input)
            .map_err(|e| format!("document.parse: ocrs get_text error: {}", e))?;

        // 3. Split into per-line strings. ocrs separates text lines with
        //    `\n`; we strip blank-but-not-empty leading/trailing
        //    whitespace by deferring to std's `lines` (which collapses
        //    trailing `\n`).
        let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();

        Ok(Self {
            bytes,
            width,
            height,
            format,
            lines,
            ocr_engine: "rten".into(),
        })
    }
}

impl DocumentBackend for ImageBackend {
    fn origin(&self) -> &'static str {
        "image"
    }
    fn pages(&self) -> Result<Value, String> {
        let mut pd = HashMap::new();
        pd.insert("page_no".into(), Value::Number(1.0));
        pd.insert("width".into(), Value::Number(self.width as f64));
        pd.insert("height".into(), Value::Number(self.height as f64));
        pd.insert("blocks".into(), Value::List(vec![]));
        Ok(Value::List(vec![Value::Dict(pd)]))
    }
    fn markdown(&self) -> Result<String, String> {
        Ok(self.lines.join("\n"))
    }
    fn text(&self) -> Result<String, String> {
        Ok(self.lines.join("\n"))
    }
    fn metadata(&self) -> Result<Value, String> {
        let mut m = HashMap::new();
        m.insert("origin".into(), Value::String("image".into()));
        m.insert("pages".into(), Value::Number(1.0));
        m.insert("format".into(), Value::String(self.format.clone()));
        m.insert("width".into(), Value::Number(self.width as f64));
        m.insert("height".into(), Value::Number(self.height as f64));
        m.insert("ocr_engine".into(), Value::String(self.ocr_engine.clone()));
        m.insert("size".into(), Value::Number(self.bytes.len() as f64));
        Ok(Value::Dict(m))
    }
    fn blocks(&self) -> Result<Value, String> {
        Ok(Value::List(vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_image() {
        let backend = ImageBackend {
            bytes: vec![],
            width: 0,
            height: 0,
            format: "png".into(),
            lines: vec![],
            ocr_engine: "rten".into(),
        };
        assert_eq!(backend.origin(), "image");
    }

    #[test]
    fn metadata_has_ocr_engine() {
        let backend = ImageBackend {
            bytes: vec![],
            width: 800,
            height: 600,
            format: "png".into(),
            lines: vec![],
            ocr_engine: "rten".into(),
        };
        let meta = backend.metadata().unwrap();
        if let Value::Dict(m) = meta {
            assert_eq!(m.get("ocr_engine"), Some(&Value::String("rten".into())));
            assert_eq!(m.get("format"), Some(&Value::String("png".into())));
        } else {
            panic!("metadata should be dict");
        }
    }

    /// v0.28 Task 6: end-to-end PNG decode + OCR pipeline against a
    /// hand-crafted "Hello World" fixture. Exercises the full
    /// `from_bytes` path: `image::ImageReader` → `ocrs::ImageSource` →
    /// `OcrEngine::prepare_input` + `get_text`. The OnceLock caches the
    /// engine after the first invocation.
    ///
    /// Skip-not-fail if the fixture is missing (mirrors the convention
    /// used elsewhere in this file so the test can run against a tree
    /// that doesn't include the fixtures dir).
    #[test]
    #[ignore = "v0.28 OCR e2e requires MORA_OCR_MODELS_DIR pointing at the v0.28 vendored models; v0.30 will add CI support for OCR e2e"]
    fn parses_real_png() {
        let path = format!("{}/tests/fixtures/sample.png", env!("CARGO_MANIFEST_DIR"));
        if !std::path::Path::new(&path).exists() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read sample.png");
        let backend = ImageBackend::from_bytes(bytes, "png".into())
            .expect("ImageBackend::from_bytes should succeed for sample.png");
        let meta = backend.metadata().expect("metadata");
        if let Value::Dict(m) = meta {
            let w = m
                .get("width")
                .and_then(|v| {
                    if let Value::Number(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .expect("width as Number");
            let h = m
                .get("height")
                .and_then(|v| {
                    if let Value::Number(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .expect("height as Number");
            assert!(w > 0.0, "width should be > 0, got {}", w);
            assert!(h > 0.0, "height should be > 0, got {}", h);
        } else {
            panic!("metadata should be a Dict");
        }
    }
}

// ===================================================================
// v0.29: user-side model-dir helpers + fail-loud
// ===================================================================

/// v0.29 tests for the user-side `.rten` model directory migration.
///
/// These tests cover the two new public APIs introduced in v0.29:
/// `user_model_path` and `require_model_file`. Both rely on env-var
/// reads, so they touch process-global state; we serialise them with a
/// `std::sync::Mutex` to prevent races with parallel `cargo test`
/// workers, AND we restore the original `MORA_OCR_MODELS_DIR` value on
/// drop (so other tests that depend on it — e.g. `parses_real_png` —
/// keep working).
#[cfg(test)]
mod v29_tests {
    use super::*;
    use std::sync::Mutex;

    /// Serial guard so two `cargo test` threads cannot simultaneously
    /// `set_var` / `remove_var` `MORA_OCR_MODELS_DIR`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard: on creation, takes the lock and snapshots the
    /// current `MORA_OCR_MODELS_DIR` value; on drop, restores it.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        original: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let original = std::env::var("MORA_OCR_MODELS_DIR").ok();
            Self {
                _lock: lock,
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: serialised by `_lock`; we are the sole writer of
            // this process-global var during the guard's lifetime.
            unsafe {
                match self.original.as_ref() {
                    Some(v) => std::env::set_var("MORA_OCR_MODELS_DIR", v),
                    None => std::env::remove_var("MORA_OCR_MODELS_DIR"),
                }
            }
        }
    }

    #[test]
    fn require_model_file_fails_loud_when_missing() {
        let _g = EnvGuard::new();
        // SAFETY: serialised by EnvGuard; EnvGuard restores the original
        // value on drop so other tests see a stable env.
        unsafe {
            std::env::set_var("MORA_OCR_MODELS_DIR", "/tmp/mora_test_nonexistent_dir_xyz");
        }
        let result = require_model_file("text-detection.rten");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("ocr.load:"),
            "error should start with ocr.load: prefix, got: {}",
            err
        );
        assert!(
            err.contains("not found"),
            "error should mention missing file, got: {}",
            err
        );
        // _g drops here → MORA_OCR_MODELS_DIR restored to original value
    }

    #[test]
    fn user_model_path_respects_mora_models_dir() {
        let _g = EnvGuard::new();
        // SAFETY: serialised by EnvGuard; restored on drop.
        unsafe {
            std::env::set_var("MORA_OCR_MODELS_DIR", "/custom/mora/ocr");
        }
        let p = user_model_path("text-detection.rten").unwrap();
        let s = p.to_string_lossy();
        assert!(s.contains("custom"), "path should contain 'custom': {}", s);
        assert!(
            s.ends_with("text-detection.rten"),
            "path should end with text-detection.rten: {}",
            s
        );
        // _g drops here → MORA_OCR_MODELS_DIR restored
    }
}
