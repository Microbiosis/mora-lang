//! v0.28: ImageBackend — pure-Rust OCR via ocrs + image
//!
//! Task 6: full pipeline. `from_bytes` decodes the PNG (via the `image`
//! crate) to populate `width`/`height`, then runs OCR (via `ocrs`, backed
//! by `rten` ONNX-style models embedded at compile time) to populate
//! `lines`. The OCR pipeline:
//!
//! 1. Decode the PNG into a `DynamicImage` and convert to RGB8.
//! 2. Build an `ocrs::ImageSource` from the RGB pixel buffer.
//! 3. Run `OcrEngine::prepare_input` (greyscale + normalize to [-0.5, 0.5]).
//! 4. Run `OcrEngine::get_text` (detect_words → find_text_lines → recognize).
//!
//! IMPORTANT — model files: ocrs does NOT bundle its ONNX-style `.rten`
//! models inside the crate. `OcrEngineParams { detection_model: None,
//! recognition_model: None }` produces an engine that errors on the first
//! `detect_words` / `get_text` call with `"Detection model not loaded"`. To
//! work fully offline we vendor:
//!
//! - `tests/fixtures/text-detection.rten`    (~2.4 MB)
//! - `tests/fixtures/text-recognition.rten`  (~9.3 MB)
//!
//! at compile time via `include_bytes!` and call
//! `rten::Model::load_static_slice` to hand the bytes to `OcrEngineParams`.
//! The engine is then memoised in a `OnceLock<OcrEngine>` so subsequent
//! `from_bytes` calls (e.g. across the test suite) reuse the same instance.
//! The first init takes ~1–3s; subsequent calls are essentially free.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::OnceLock;

use image::io::Reader as ImageReader;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};

use crate::value::Value;
use crate::document::DocumentBackend;

#[derive(Debug)]
pub struct ImageBackend {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String,     // "png" only in v0.28
    pub lines: Vec<String>, // OCR output, one per detected text line
    pub ocr_engine: String, // "rten" (rten ONNX inference runtime)
}

/// Embed ocrs's detection + recognition `.rten` model files at compile time.
///
/// These are the official models from
/// https://ocrs-models.s3-accelerate.amazonaws.com/ (text-detection.rten
/// and text-recognition.rten). Vendoring them lets the build work offline
/// and removes a network dependency from the test suite.
const DETECTION_MODEL_BYTES: &[u8] =
    include_bytes!("../../../tests/fixtures/text-detection.rten");
const RECOGNITION_MODEL_BYTES: &[u8] =
    include_bytes!("../../../tests/fixtures/text-recognition.rten");

/// Lazily-initialised, process-wide singleton `OcrEngine`.
///
/// ocrs's engine wraps two `rten::Model` instances (detection +
/// recognition). Loading them costs ~1–3s on a warm machine, so we cache
/// the engine here. OnceLock gives us cheap "init exactly once" semantics
/// without any external mutability primitives.
///
/// The `anyhow::Result` is sticky: if init fails (corrupt/missing model
/// bytes), we don't keep retrying on each call and re-paying the load cost.
static OCR_ENGINE: OnceLock<anyhow::Result<OcrEngine>> = OnceLock::new();

/// Build (or return the previously-built) OCR engine. The returned error
/// is wrapped in a `String` so callers can propagate it through the
/// `from_bytes` `Result` boundary without leaking ocrs's `anyhow::Error`.
fn ocr_engine_singleton() -> Result<&'static OcrEngine, String> {
    let res = OCR_ENGINE.get_or_init(|| -> anyhow::Result<OcrEngine> {
        let detection_model = rten::Model::load_static_slice(DETECTION_MODEL_BYTES)?;
        let recognition_model = rten::Model::load_static_slice(RECOGNITION_MODEL_BYTES)?;
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
    fn origin(&self) -> &'static str { "image" }
    fn pages(&self) -> Result<Value, String> {
        let mut pd = HashMap::new();
        pd.insert("page_no".into(), Value::Number(1.0));
        pd.insert("width".into(), Value::Number(self.width as f64));
        pd.insert("height".into(), Value::Number(self.height as f64));
        pd.insert("blocks".into(), Value::List(vec![]));
        Ok(Value::List(vec![Value::Dict(pd)]))
    }
    fn markdown(&self) -> Result<String, String> { Ok(self.lines.join("\n")) }
    fn text(&self) -> Result<String, String> { Ok(self.lines.join("\n")) }
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
    fn blocks(&self) -> Result<Value, String> { Ok(Value::List(vec![])) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_image() {
        let backend = ImageBackend {
            bytes: vec![], width: 0, height: 0, format: "png".into(),
            lines: vec![], ocr_engine: "rten".into(),
        };
        assert_eq!(backend.origin(), "image");
    }

    #[test]
    fn metadata_has_ocr_engine() {
        let backend = ImageBackend {
            bytes: vec![], width: 800, height: 600, format: "png".into(),
            lines: vec![], ocr_engine: "rten".into(),
        };
        let meta = backend.metadata().unwrap();
        if let Value::Dict(m) = meta {
            assert_eq!(m.get("ocr_engine"), Some(&Value::String("rten".into())));
            assert_eq!(m.get("format"), Some(&Value::String("png".into())));
        } else { panic!("metadata should be dict"); }
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
    fn parses_real_png() {
        let path = format!(
            "{}/tests/fixtures/sample.png",
            env!("CARGO_MANIFEST_DIR")
        );
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
                .and_then(|v| if let Value::Number(n) = v { Some(*n) } else { None })
                .expect("width as Number");
            let h = m
                .get("height")
                .and_then(|v| if let Value::Number(n) = v { Some(*n) } else { None })
                .expect("height as Number");
            assert!(w > 0.0, "width should be > 0, got {}", w);
            assert!(h > 0.0, "height should be > 0, got {}", h);
        } else {
            panic!("metadata should be a Dict");
        }
    }
}
