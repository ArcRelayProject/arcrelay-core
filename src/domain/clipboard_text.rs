use crate::domain::clipboard::{ClipboardPasteMode, ClipboardPayload, ClipboardTextSyntax};
use crate::domain::paste_text_detector::TextDetector;
use crate::error::{Error, Result};

pub fn detect_text_syntax(text: &str) -> ClipboardTextSyntax {
    TextDetector::detect(text)
}

pub fn convert_payload(
    payload: &ClipboardPayload,
    mode: ClipboardPasteMode,
) -> Result<ClipboardPayload> {
    if mode == ClipboardPasteMode::Source {
        return Ok(payload.clone());
    }
    let text = match payload {
        ClipboardPayload::Text(text) => text.clone(),
        ClipboardPayload::RichText { plain_text, .. } => plain_text.clone(),
        ClipboardPayload::Image { .. } | ClipboardPayload::Files(_) => {
            return Err(Error::Clipboard(
                "this paste format is only available for text records".into(),
            ));
        }
    };
    match mode {
        ClipboardPasteMode::Source => unreachable!(),
        ClipboardPasteMode::PlainText => Ok(ClipboardPayload::Text(text)),
        ClipboardPasteMode::RichText => match payload {
            ClipboardPayload::RichText { .. } => Ok(payload.clone()),
            _ => Err(Error::Clipboard(
                "the clipboard record has no rich-text source format".into(),
            )),
        },
        ClipboardPasteMode::JsonCompact => Ok(ClipboardPayload::Text(json_text(&text, false)?)),
        ClipboardPasteMode::JsonFormatted => Ok(ClipboardPayload::Text(json_text(&text, true)?)),
        ClipboardPasteMode::Yaml => {
            let value = parse_structured_text(&text)?;
            let value = serde_yaml::to_string(&value)
                .map_err(|error| Error::Clipboard(format!("convert to YAML: {error}")))?;
            Ok(ClipboardPayload::Text(
                value.trim_start_matches("---\n").to_string(),
            ))
        }
    }
}

fn json_text(text: &str, pretty: bool) -> Result<String> {
    let value = parse_structured_text(text)?;
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|error| Error::Clipboard(format!("convert to JSON: {error}")))
}

fn parse_structured_text(text: &str) -> Result<serde_json::Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Ok(value);
    }
    let yaml: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|error| Error::Clipboard(format!("invalid JSON or YAML: {error}")))?;
    serde_json::to_value(yaml)
        .map_err(|error| Error::Clipboard(format!("convert YAML value: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_converts_json_and_yaml() {
        assert_eq!(
            detect_text_syntax(r#"{"name":"ArcRelay"}"#),
            ClipboardTextSyntax::Json
        );
        assert_eq!(
            detect_text_syntax("name: ArcRelay\nenabled: true"),
            ClipboardTextSyntax::Yaml
        );
        let yaml = convert_payload(
            &ClipboardPayload::Text(r#"{"name":"ArcRelay"}"#.into()),
            ClipboardPasteMode::Yaml,
        )
        .unwrap();
        assert!(matches!(yaml, ClipboardPayload::Text(value) if value.contains("name: ArcRelay")));
        let json = convert_payload(
            &ClipboardPayload::Text("name: ArcRelay\nenabled: true".into()),
            ClipboardPasteMode::JsonFormatted,
        )
        .unwrap();
        assert!(matches!(json, ClipboardPayload::Text(value) if value.contains("\"name\"")));
    }

    #[test]
    fn rich_text_can_be_flattened_without_losing_source() {
        let payload = ClipboardPayload::RichText {
            html: "<b>Hello</b>".into(),
            plain_text: "Hello".into(),
            rtf: Some("{\\rtf1 Hello}".into()),
        };
        assert_eq!(
            convert_payload(&payload, ClipboardPasteMode::PlainText).unwrap(),
            ClipboardPayload::Text("Hello".into())
        );
        assert_eq!(
            convert_payload(&payload, ClipboardPasteMode::Source).unwrap(),
            payload
        );
    }
}
