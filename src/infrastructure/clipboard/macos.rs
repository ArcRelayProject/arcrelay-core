//! Native writes carry an origin marker in the same pasteboard item as the
//! content. Received sync writes are host-only, so Handoff cannot echo them.
use super::*;
use objc2::{rc::autoreleasepool, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypeFileURL,
    NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

const WRITE_MARKER: &str = "cn.arcrelay.clipboard.write";
fn write_identity() -> &'static str {
    static IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

const HANDOFF_MARKER: &str = "com.apple.is-remote-clipboard";

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    count: isize,
    pub origin: NativeClipboardOrigin,
}

fn stamp_for(board: &NSPasteboard) -> Stamp {
    let count = board.changeCount();
    let types = board.types();
    let has = |name: &str| {
        types
            .as_ref()
            .is_some_and(|types| types.containsObject(&NSString::from_str(name)))
    };
    Stamp {
        count,
        origin: if has(WRITE_MARKER)
            && board
                .stringForType(&NSString::from_str(WRITE_MARKER))
                .is_some_and(|value| value.to_string() == write_identity())
        {
            NativeClipboardOrigin::ArcRelay
        } else if has(HANDOFF_MARKER) {
            NativeClipboardOrigin::Handoff
        } else if has(RUSTDESK_MARKER) {
            NativeClipboardOrigin::RustDesk
        } else {
            NativeClipboardOrigin::Local
        },
    }
}

pub(super) fn stamp() -> Stamp {
    autoreleasepool(|_| stamp_for(&NSPasteboard::generalPasteboard()))
}

pub(super) fn write_payload(payload: ClipboardPayload, local_only: bool) -> Result<()> {
    autoreleasepool(|_| write_to(&NSPasteboard::generalPasteboard(), payload, local_only))
}

fn write_to(board: &NSPasteboard, payload: ClipboardPayload, local_only: bool) -> Result<()> {
    let item = NSPasteboardItem::new();
    let mut items = Vec::new();
    let set_string = |item: &NSPasteboardItem, value: &str, kind: &NSString| {
        item.setString_forType(&NSString::from_str(value), kind)
    };
    // Build all representations before clearing the user's current clipboard.
    let valid = match payload {
        ClipboardPayload::Text(text) => set_string(&item, &text, unsafe { NSPasteboardTypeString }),
        ClipboardPayload::RichText {
            html,
            plain_text,
            rtf,
        } => {
            // HTML-only records (including older synchronized entries) still
            // need a usable representation for plain-text editors.
            let plain_text = if plain_text.is_empty() {
                strip_html(&html)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                plain_text
            };
            let mut ok = set_string(&item, &plain_text, unsafe { NSPasteboardTypeString });
            if !html.is_empty() {
                ok &= set_string(&item, &html, unsafe { NSPasteboardTypeHTML });
            }
            if let Some(rtf) = rtf.filter(|rtf| !rtf.is_empty()) {
                ok &= item.setData_forType(&NSData::with_bytes(rtf.as_bytes()), unsafe {
                    NSPasteboardTypeRTF
                });
            }
            ok
        }
        ClipboardPayload::Image { png, .. } => {
            decode_image_png(&png)?;
            item.setData_forType(&NSData::with_bytes(&png), unsafe { NSPasteboardTypePNG })
        }
        ClipboardPayload::Files(paths) => {
            for path in paths {
                let path = path.strip_prefix("file://").unwrap_or(&path);
                if !Path::new(path).exists() {
                    continue;
                }
                let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                let file = NSPasteboardItem::new();
                let Some(url) = url.absoluteString() else {
                    continue;
                };
                if !file.setString_forType(&url, unsafe { NSPasteboardTypeFileURL }) {
                    return Err(Error::Clipboard("prepare clipboard file URL failed".into()));
                }
                items.push(file);
            }
            !items.is_empty()
        }
    };
    if !valid {
        return Err(Error::Clipboard("prepare clipboard content failed".into()));
    }
    if items.is_empty() {
        items.push(item);
    }
    for item in &items {
        if !set_string(item, write_identity(), &NSString::from_str(WRITE_MARKER)) {
            return Err(Error::Clipboard("prepare clipboard origin failed".into()));
        }
    }
    let objects = NSArray::from_retained_slice(
        &items
            .iter()
            .cloned()
            .map(ProtocolObject::from_retained)
            .collect::<Vec<_>>(),
    );
    // clearContents after this call would reset CurrentHostOnly. Write objects
    // directly instead of clipboard-rs's setters, which clear a second time.
    board.prepareForNewContentsWithOptions(if local_only {
        NSPasteboardContentsOptions::CurrentHostOnly
    } else {
        NSPasteboardContentsOptions::empty()
    });
    if !board.writeObjects(&objects) {
        return Err(Error::Clipboard("write clipboard objects failed".into()));
    }
    // All representations were supplied on owned NSPasteboardItems before
    // writeObjects. AppKit reports whether it accepted the complete array.
    // Reading it back here is unsafe: another process (including Handoff) can
    // replace the pasteboard while AppKit updates its type cache, and macOS 15
    // has crashed inside dataForType instead of returning nil. Paste callers
    // still check changeCount before posting the shortcut.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_paste_writes_file_urls_without_image_representations() {
        autoreleasepool(|_| {
            for mode in [ClipboardPasteMode::ImageJpg, ClipboardPasteMode::ImagePng] {
                let mut png = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(2, 2, image::Rgba([50, 100, 150, 128]))
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                let payload = prepare_image_file(
                    ClipboardPayload::Image {
                        png: png.into_inner(),
                        width: 2,
                        height: 2,
                    },
                    mode,
                )
                .unwrap();
                let ClipboardPayload::Files(ref paths) = payload else {
                    panic!("expected files")
                };
                let path = paths[0].clone();
                let board = NSPasteboard::pasteboardWithUniqueName();
                write_to(&board, payload, true).unwrap();
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
                assert_eq!(
                    board
                        .stringForType(unsafe { NSPasteboardTypeFileURL })
                        .unwrap(),
                    url.absoluteString().unwrap(),
                );
                for absent in ["public.jpeg", "public.png"] {
                    assert!(board.dataForType(&NSString::from_str(absent)).is_none());
                }
                assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::ArcRelay);
                board.clearContents();
                std::fs::remove_file(path).unwrap();
            }
        });
    }

    #[test]
    fn large_html_write_keeps_plain_text_fallback() {
        autoreleasepool(|_| {
            let board = NSPasteboard::pasteboardWithUniqueName();
            let html = format!("<p>{}</p>", "大数据🙂".repeat(300_000));
            write_to(
                &board,
                ClipboardPayload::RichText {
                    html: html.clone(),
                    plain_text: "fallback".into(),
                    rtf: None,
                },
                false,
            )
            .unwrap();
            assert_eq!(
                board
                    .dataForType(unsafe { NSPasteboardTypeHTML })
                    .unwrap()
                    .to_vec(),
                html.as_bytes()
            );
            assert_eq!(
                board
                    .stringForType(unsafe { NSPasteboardTypeString })
                    .unwrap()
                    .to_string(),
                "fallback"
            );
            board.clearContents();
        });
    }

    #[test]
    fn html_without_plain_text_remains_pasteable_in_text_editors() {
        autoreleasepool(|_| {
            let board = NSPasteboard::pasteboardWithUniqueName();
            write_to(
                &board,
                ClipboardPayload::RichText {
                    html: "<p>Hello <b>世界</b></p>".into(),
                    plain_text: String::new(),
                    rtf: None,
                },
                false,
            )
            .unwrap();
            assert_eq!(
                board
                    .stringForType(unsafe { NSPasteboardTypeString })
                    .unwrap()
                    .to_string(),
                "Hello 世界"
            );
            assert_eq!(
                board
                    .dataForType(unsafe { NSPasteboardTypeHTML })
                    .unwrap()
                    .to_vec(),
                "<p>Hello <b>世界</b></p>".as_bytes()
            );
            board.clearContents();
        });
    }

    #[test]
    fn rustdesk_marker_in_a_separate_item_identifies_remote_writes() {
        autoreleasepool(|_| {
            let board = NSPasteboard::pasteboardWithUniqueName();
            let owner = NSPasteboardItem::new();
            owner.setData_forType(
                &NSData::with_bytes(&[1]),
                &NSString::from_str(RUSTDESK_MARKER),
            );
            let content = NSPasteboardItem::new();
            content.setString_forType(&NSString::from_str("remote content"), unsafe {
                NSPasteboardTypeString
            });
            board.clearContents();
            assert!(board.writeObjects(&NSArray::from_retained_slice(&[
                ProtocolObject::from_retained(owner),
                ProtocolObject::from_retained(content),
            ])));
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::RustDesk);
            // An explicit application copy removes the relay marker.
            board.clearContents();
            board.setString_forType(&NSString::from_str("remote content"), unsafe {
                NSPasteboardTypeString
            });
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::Local);
            board.clearContents();
        });
    }

    #[test]
    fn native_writes_keep_representations_and_origin_without_a_runtime() {
        autoreleasepool(|_| {
            // A private pasteboard never modifies the user's general clipboard.
            let board = NSPasteboard::pasteboardWithUniqueName();
            write_to(
                &board,
                ClipboardPayload::RichText {
                    html: "<b>Hello</b>".into(),
                    plain_text: "Hello".into(),
                    rtf: Some("{\\rtf1 Hello}".into()),
                },
                true,
            )
            .unwrap();
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::ArcRelay);
            assert_eq!(
                board
                    .stringForType(unsafe { NSPasteboardTypeString })
                    .unwrap()
                    .to_string(),
                "Hello"
            );
            assert_eq!(
                board
                    .stringForType(unsafe { NSPasteboardTypeHTML })
                    .unwrap()
                    .to_string(),
                "<b>Hello</b>"
            );
            assert!(board.dataForType(unsafe { NSPasteboardTypeRTF }).is_some());
            // An explicit application copy clears the marker, including when
            // the text is identical to the remote write.
            board.clearContents();
            board.setString_forType(&NSString::from_str("Hello"), unsafe {
                NSPasteboardTypeString
            });
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::Local);
            // Another Mac's panel write can arrive through Handoff. Its marker
            // must not be mistaken for a write performed by this process.
            let handoff = NSPasteboardItem::new();
            handoff.setString_forType(
                &NSString::from_str("another-instance"),
                &NSString::from_str(WRITE_MARKER),
            );
            handoff.setString_forType(
                &NSString::from_str("1"),
                &NSString::from_str(HANDOFF_MARKER),
            );
            handoff.setString_forType(&NSString::from_str("Hello"), unsafe {
                NSPasteboardTypeString
            });
            board.clearContents();
            assert!(board.writeObjects(&NSArray::from_retained_slice(&[
                ProtocolObject::from_retained(handoff)
            ])));
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::Handoff);
            board.clearContents();
        });
    }
    #[test]
    fn native_file_and_image_writes_preserve_content_and_failed_prepare_is_inert() {
        autoreleasepool(|_| {
            let board = NSPasteboard::pasteboardWithUniqueName();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("file with spaces.txt");
            std::fs::write(&file, "test").unwrap();
            write_to(
                &board,
                ClipboardPayload::Files(vec![file.to_string_lossy().into_owned()]),
                false,
            )
            .unwrap();
            assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::ArcRelay);
            assert!(board
                .stringForType(unsafe { NSPasteboardTypeFileURL })
                .unwrap()
                .to_string()
                .contains("file%20with%20spaces.txt"));
            let mut png = Vec::new();
            image::DynamicImage::new_rgb8(2, 2)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            write_to(
                &board,
                ClipboardPayload::Image {
                    png: png.clone(),
                    width: 0,
                    height: 0,
                },
                true,
            )
            .unwrap();
            assert_eq!(
                board
                    .dataForType(unsafe { NSPasteboardTypePNG })
                    .unwrap()
                    .to_vec(),
                png
            );
            let before = stamp_for(&board);
            assert!(write_to(
                &board,
                ClipboardPayload::Image {
                    png: vec![1, 2],
                    width: 1,
                    height: 1
                },
                true
            )
            .is_err());
            assert_eq!(stamp_for(&board), before);
            board.clearContents();
        });
    }
}

// The marker and secret share a single local-only item. Receipts contain no secret.
pub(super) fn write_private(text: &str, receipt: &str) -> Result<String> {
    autoreleasepool(|_| write_private_to(&NSPasteboard::generalPasteboard(), text, receipt))
}
fn write_private_to(board: &NSPasteboard, text: &str, receipt: &str) -> Result<String> {
    let item = NSPasteboardItem::new();
    if !item.setString_forType(
        &NSString::from_str(write_identity()),
        &NSString::from_str(WRITE_MARKER),
    ) || !item.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
        || !item.setString_forType(
            &NSString::from_str(receipt),
            &NSString::from_str(PRIVATE_MARKER),
        )
        || !item.setString_forType(
            &NSString::from_str("1"),
            &NSString::from_str("org.nspasteboard.ConcealedType"),
        )
    {
        return Err(Error::Clipboard(
            "private clipboard preparation failed".into(),
        ));
    }
    board.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    let object = ProtocolObject::from_ref(&*item);
    let items = NSArray::from_slice(&[object]);
    if !board.writeObjects(&items) {
        return Err(Error::Clipboard("private clipboard write failed".into()));
    }
    Ok(format!("{}:{}", receipt, board.changeCount()))
}
pub(super) fn clear_private(receipt: &str) -> bool {
    autoreleasepool(|_| clear_private_from(&NSPasteboard::generalPasteboard(), receipt))
}
fn clear_private_from(board: &NSPasteboard, receipt: &str) -> bool {
    let Some((receipt, expected)) = receipt.rsplit_once(':') else {
        return false;
    };
    let count = board.changeCount();
    if expected.parse::<isize>().ok() != Some(count) {
        return false;
    }
    if board
        .stringForType(&NSString::from_str(PRIVATE_MARKER))
        .map(|v| v.to_string())
        .as_deref()
        != Some(receipt)
        || board.changeCount() != count
    {
        return false;
    }
    board.clearContents();
    true
}
#[cfg(test)]
#[test]
fn login_private_copy_is_suppressed_and_cleanup_preserves_newer_same_text() {
    autoreleasepool(|_| {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let receipt = write_private_to(&board, "private-test-value", "test-receipt").unwrap();
        assert_eq!(stamp_for(&board).origin, NativeClipboardOrigin::ArcRelay);
        assert!(board
            .types()
            .unwrap()
            .iter()
            .any(|t| t.to_string() == PRIVATE_MARKER));
        assert!(clear_private_from(&board, &receipt));
        let receipt = write_private_to(&board, "same-text", "test-receipt").unwrap();
        board.clearContents();
        board.setString_forType(&NSString::from_str("same-text"), unsafe {
            NSPasteboardTypeString
        });
        assert!(!clear_private_from(&board, &receipt));
        assert_eq!(
            board
                .stringForType(unsafe { NSPasteboardTypeString })
                .unwrap()
                .to_string(),
            "same-text"
        );
        board.clearContents();
    });
}
