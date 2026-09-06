# Clipboard OCR models

These runtime models are bundled for ArcRelay's local image-to-text clipboard
paste feature and loaded by `ocr-rs` 2.4.1.

- Model family: PP-OCRv6 tiny (Simplified/Traditional Chinese, English, and
  Latin-script languages; Japanese is not supported by the tiny tier)
- Source: <https://github.com/zibo-chen/rust-paddle-ocr/tree/d7d4e4f2f5cebea6d1423fa85fcb5962eb73b38b/models>
- Source release: `v2.4.1`
- Upstream licenses: Apache-2.0 (`ocr-rs` and PaddleOCR)

SHA-256 checksums are recorded after the model files are materialized:

```text
PP-OCRv6_tiny_det.mnn  7fab7b858f136bc93a760bdca66aaf25f0ff10accabb31e6ef853a897fb9cfec
PP-OCRv6_tiny_rec.mnn  0a43c3c979a98b905f5e84913209998f510189419b5a5d4152bbb01ce8d17a93
ppocr_keys_v6_tiny.txt c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd
```
