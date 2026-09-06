use std::io::Cursor;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use image::{imageops::FilterType, DynamicImage, ImageFormat, ImageReader, Limits};
use ocr_rs::{OcrEngine, OcrEngineConfig, RecognizeOptions, RotatedTextMode};

use crate::domain::clipboard::{
    ClipboardImageOcr, ClipboardOcrBlock, ClipboardOcrCharacter, ClipboardOcrPoint,
};
use crate::error::{Error, Result};

const DETECTION_MODEL: &[u8] = include_bytes!("../../models/ocr/PP-OCRv6_tiny_det.mnn");
const RECOGNITION_MODEL: &[u8] = include_bytes!("../../models/ocr/PP-OCRv6_tiny_rec.mnn");
const CHARACTER_SET: &[u8] = include_bytes!("../../models/ocr/ppocr_keys_v6_tiny.txt");

const MAX_OCR_IMAGE_DIMENSION: u32 = 16_384;
const MAX_OCR_DECODE_BYTES: u64 = 192 * 1024 * 1024;
const MAX_OCR_INFERENCE_PIXELS: u64 = 3_000_000;
pub(super) const OCR_MODEL_VERSION: &str = "PP-OCRv6-tiny/ocr-rs-2.4.1+ctc-alignment-v1";

struct OcrState {
    engine: Option<OcrEngine>,
    last_used: Option<Instant>,
}

/// Lazily initialized, process-local OCR engine for clipboard images.
///
/// The background worker serializes recognition through this lock. It loads
/// the MNN models only when a queued image actually needs OCR and can drop the
/// engine after the queue has been idle, releasing the model/session memory.
pub(super) struct ClipboardOcr {
    resources: std::sync::Arc<arcrelay_content::ContentResources>,
    state: Mutex<OcrState>,
}

impl ClipboardOcr {
    pub(super) fn with_resources(
        resources: std::sync::Arc<arcrelay_content::ContentResources>,
    ) -> Self {
        Self {
            resources,
            state: Mutex::new(OcrState {
                engine: None,
                last_used: None,
            }),
        }
    }

    pub(super) fn recognize_png(&self, png: &[u8]) -> Result<ClipboardImageOcr> {
        let _work = self
            .resources
            .blocking_work(MAX_OCR_DECODE_BYTES + 64 * 1024 * 1024)
            .map_err(|error| Error::Clipboard(error.to_string()))?;
        let original = decode_png(png)?;
        let original_width = original.width();
        let original_height = original.height();
        let image = resize_for_inference(original);
        let x_scale = original_width as f32 / image.width() as f32;
        let y_scale = original_height as f32 / image.height() as f32;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Clipboard("clipboard OCR lock poisoned".into()))?;
        if state.engine.is_none() {
            state.engine = Some(
                OcrEngine::from_bytes(
                    DETECTION_MODEL,
                    RECOGNITION_MODEL,
                    CHARACTER_SET,
                    Some(OcrEngineConfig::default()),
                )
                .map_err(|error| {
                    Error::Clipboard(format!("initialize clipboard OCR engine: {error}"))
                })?,
            );
        }

        // Re-recognize tall regions in both orientations without paying for
        // the two extra full-image detection passes used by Robust mode.
        let options = RecognizeOptions::new().with_rotated_text_mode(RotatedTextMode::DetectedOnly);
        let recognition = state
            .engine
            .as_ref()
            .expect("clipboard OCR engine was initialized")
            .recognize_with_options_and_alignment(&image, &options);
        state.last_used = Some(Instant::now());
        drop(state);
        let mut results = recognition
            .map_err(|error| Error::Clipboard(format!("recognize clipboard image: {error}")))?;
        results.sort_by(|left, right| {
            left.bbox
                .rect
                .top()
                .cmp(&right.bbox.rect.top())
                .then_with(|| left.bbox.rect.left().cmp(&right.bbox.rect.left()))
        });
        let blocks = results
            .into_iter()
            .filter_map(|result| {
                let text = result.text.trim().to_string();
                let trimmed_start = result
                    .text
                    .chars()
                    .take_while(|character| character.is_whitespace())
                    .count();
                let trimmed_len = text.chars().count();
                (!text.is_empty()).then(|| ClipboardOcrBlock {
                    text,
                    confidence: result.confidence,
                    left: scale_i32(result.bbox.rect.left(), x_scale),
                    top: scale_i32(result.bbox.rect.top(), y_scale),
                    width: scale_u32(result.bbox.rect.width(), x_scale),
                    height: scale_u32(result.bbox.rect.height(), y_scale),
                    points: result.bbox.points.map(|points| {
                        points.map(|point| ClipboardOcrPoint {
                            x: point.x * x_scale,
                            y: point.y * y_scale,
                        })
                    }),
                    characters: result
                        .characters
                        .into_iter()
                        .skip(trimmed_start)
                        .take(trimmed_len)
                        .map(|character| ClipboardOcrCharacter {
                            text: character.character.to_string(),
                            confidence: character.confidence,
                            points: character.points.map(|point| ClipboardOcrPoint {
                                x: point.x * x_scale,
                                y: point.y * y_scale,
                            }),
                        })
                        .collect(),
                })
            })
            .collect::<Vec<_>>();
        let text = blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ClipboardImageOcr {
            text,
            blocks,
            model_version: OCR_MODEL_VERSION.into(),
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    /// Drops the model and native inference session after the caller-observed
    /// idle period. Returns whether memory was released.
    pub(super) fn release_if_idle(&self, idle_for: Duration) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let should_release = state.engine.is_some()
            && state
                .last_used
                .is_some_and(|last_used| last_used.elapsed() >= idle_for);
        if should_release {
            state.engine = None;
            state.last_used = None;
        }
        should_release
    }
}

fn resize_for_inference(image: DynamicImage) -> DynamicImage {
    let width = image.width();
    let height = image.height();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels <= MAX_OCR_INFERENCE_PIXELS {
        return image;
    }
    let ratio = (MAX_OCR_INFERENCE_PIXELS as f64 / pixels as f64).sqrt();
    let resized_width = (f64::from(width) * ratio).round().max(1.0) as u32;
    let resized_height = (f64::from(height) * ratio).round().max(1.0) as u32;
    image.resize(resized_width, resized_height, FilterType::Triangle)
}

fn scale_i32(value: i32, scale: f32) -> i32 {
    (value as f64 * f64::from(scale))
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

fn scale_u32(value: u32, scale: f32) -> u32 {
    (f64::from(value) * f64::from(scale))
        .round()
        .clamp(0.0, u32::MAX as f64) as u32
}

fn decode_png(png: &[u8]) -> Result<image::DynamicImage> {
    if png.is_empty() {
        return Err(Error::Clipboard("clipboard image data is empty".into()));
    }
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_OCR_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_OCR_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_OCR_DECODE_BYTES);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| Error::Clipboard(format!("decode clipboard image for OCR: {error}")))
}
