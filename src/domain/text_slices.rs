//! Clipboard text boundaries and selection assembly. Public offsets are UTF-16
//! code units for DOM selections; all internal slicing uses UTF-8 boundaries.
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::LazyLock,
};
use unicode_segmentation::UnicodeSegmentation;

const LIMIT: usize = 800;
const MAX_UTF16: usize = 120_000;
const LEVELS: [&str; 5] = ["document", "block", "sentence", "phrase", "word"];

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TextSlice {
    pub id: String,
    pub level: TextSliceLevel,
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TextSliceModel {
    pub text: String,
    pub version: String,
    pub levels: TextSliceLevels,
    pub truncated: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum TextSliceLevel {
    Document,
    Block,
    Sentence,
    Phrase,
    Word,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct TextSliceLevels {
    pub document: Vec<TextSlice>,
    pub block: Vec<TextSlice>,
    pub sentence: Vec<TextSlice>,
    pub phrase: Vec<TextSlice>,
    pub word: Vec<TextSlice>,
}
impl TextSliceLevels {
    pub fn values(&self) -> impl Iterator<Item = &Vec<TextSlice>> {
        [
            &self.document,
            &self.block,
            &self.sentence,
            &self.phrase,
            &self.word,
        ]
        .into_iter()
    }
}
impl std::ops::Index<&str> for TextSliceLevels {
    type Output = Vec<TextSlice>;
    fn index(&self, level: &str) -> &Self::Output {
        match level {
            "document" => &self.document,
            "block" => &self.block,
            "sentence" => &self.sentence,
            "phrase" => &self.phrase,
            "word" => &self.word,
            _ => panic!("unknown text slice level"),
        }
    }
}
static ATOMS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#"(?i)\b(?:https?|ftp)://[^\s<>"']+"#,
        r#"(?i)\bmagnet:\?[^\s<>"']+"#,
        r"[\p{L}\p{N}._%+-]+@[\p{L}\p{N}.-]+\.[\p{L}]{2,}",
        r#"(?:[A-Za-z]:\\|/)[^\s<>"|]+"#,
        r"[@#][\p{L}\p{N}_][\p{L}\p{N}_.-]*",
        r"\b[\p{L}\p{N}]+(?:[._+@/\\#$%^&*=-][\p{L}\p{N}]+)+\b[!?]?",
    ]
    .into_iter()
    .map(|s| Regex::new(s).expect("constant atom expression"))
    .collect()
});
static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["“][^"”\r\n]*["”]|['‘][^'’\r\n]*['’]|「[^」\r\n]*」|『[^』\r\n]*』"#).unwrap()
});
static GAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\s\p{P}\p{S}]*$").unwrap());

fn trim(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let source = &text[range.clone()];
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = range.start + source.len() - source.trim_start().len();
    Some(start..start + trimmed.len())
}
fn atoms(text: &str, range: Range<usize>, identifiers: bool) -> Vec<Range<usize>> {
    let mut result = ATOMS
        .iter()
        .take(if identifiers { 6 } else { 5 })
        .flat_map(|regex| {
            regex
                .find_iter(&text[range.clone()])
                .map(|m| range.start + m.start()..range.start + m.end())
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut end = 0;
    result.retain(|r| {
        if r.start < end {
            false
        } else {
            end = r.end;
            true
        }
    });
    result
}
fn phrases(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut protected = atoms(text, range.clone(), false);
    protected.extend(
        QUOTED
            .find_iter(&text[range.clone()])
            .map(|m| range.start + m.start()..range.start + m.end()),
    );
    protected.sort_by_key(|r| r.start);
    let mut result = Vec::new();
    let mut start = range.start;
    let mut protected_index = 0;
    for (i, ch) in text[range.clone()].char_indices() {
        let i = i + range.start;
        while protected.get(protected_index).is_some_and(|r| r.end <= i) {
            protected_index += 1;
        }
        if !",，;；:：=|、•\t".contains(ch)
            || protected
                .get(protected_index)
                .is_some_and(|r| r.contains(&i))
        {
            continue;
        }
        if let Some(r) = trim(text, start..i) {
            result.push(r);
        }
        if result.len() >= LIMIT {
            return result;
        }
        start = i + ch.len_utf8();
    }
    if let Some(r) = trim(text, start..range.end) {
        result.push(r);
    }
    result
}
fn words(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut start = range.start;
    for atom in atoms(text, range.clone(), true) {
        result.extend(
            text[start..atom.start]
                .unicode_word_indices()
                .take(LIMIT)
                .map(|(i, w)| start + i..start + i + w.len()),
        );
        start = atom.end;
        result.push(atom);
        if result.len() >= LIMIT {
            result.truncate(LIMIT);
            return result;
        }
    }
    result.extend(
        text[start..range.end]
            .unicode_word_indices()
            .take(LIMIT - result.len())
            .map(|(i, w)| start + i..start + i + w.len()),
    );
    result
}

pub fn build_text_slice_model(text: &str) -> TextSliceModel {
    let mut units = 0;
    let mut boundary = text.len();
    for (i, c) in text.char_indices() {
        if units + c.len_utf16() > MAX_UTF16 {
            boundary = i;
            break;
        }
        units += c.len_utf16();
    }
    // Keep combining sequences intact at the detailed segmentation limit.
    if boundary < text.len() {
        boundary = text
            .grapheme_indices(true)
            .take_while(|(i, _)| *i <= boundary)
            .last()
            .map_or(0, |(i, _)| i);
    }
    let mut ranges: [Vec<Range<usize>>; 5] = std::array::from_fn(|_| Vec::new());
    ranges[0] = trim(text, 0..text.len()).into_iter().collect();
    let mut start = 0;
    for line in text[..boundary].split_inclusive(['\r', '\n']) {
        if let Some(r) = trim(text, start..start + line.len()) {
            ranges[1].push(r);
        }
        start += line.len();
        if ranges[1].len() >= LIMIT {
            break;
        }
    }
    for level in 2..5 {
        let mut next = Vec::new();
        for parent in &ranges[level - 1] {
            let children = match level {
                2 => text[parent.clone()]
                    .split_sentence_bound_indices()
                    .filter_map(|(i, s)| trim(text, parent.start + i..parent.start + i + s.len()))
                    .take(LIMIT)
                    .collect(),
                3 => phrases(text, parent.clone()),
                _ => words(text, parent.clone()),
            };
            next.extend(children.into_iter().take(LIMIT - next.len()));
            if next.len() >= LIMIT {
                break;
            }
        }
        ranges[level] = next;
    }
    let truncated = boundary < text.len() || ranges.iter().any(|r| r.len() >= LIMIT);
    // Store only emitted boundaries, never allocate a full byte-index table for
    // a large document whose detailed view is already capped.
    let wanted: HashSet<usize> = ranges
        .iter()
        .flatten()
        .flat_map(|r| [r.start, r.end])
        .collect();
    let mut utf16 = HashMap::with_capacity(wanted.len());
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if wanted.contains(&i) {
            utf16.insert(i, units);
        }
        units += c.len_utf16();
    }
    utf16.insert(text.len(), units);
    let mut levels = LEVELS.into_iter().zip(ranges).map(|(level, ranges)| {
        let mut seen = HashSet::new();
        let slices = ranges
            .into_iter()
            .filter(|r| seen.insert((r.start, r.end)))
            .map(|r| TextSlice {
                id: format!("{level}-{}-{}", utf16[&r.start], utf16[&r.end]),
                level: match level {
                    "document" => TextSliceLevel::Document,
                    "block" => TextSliceLevel::Block,
                    "sentence" => TextSliceLevel::Sentence,
                    "phrase" => TextSliceLevel::Phrase,
                    _ => TextSliceLevel::Word,
                },
                start: utf16[&r.start],
                end: utf16[&r.end],
                text: text[r].into(),
            })
            .collect();
        slices
    });
    let levels = TextSliceLevels {
        document: levels.next().unwrap(),
        block: levels.next().unwrap(),
        sentence: levels.next().unwrap(),
        phrase: levels.next().unwrap(),
        word: levels.next().unwrap(),
    };
    TextSliceModel {
        text: text.into(),
        version: format!("{:x}", Sha256::digest(text.as_bytes())),
        levels,
        truncated,
    }
}

/// Selection IDs belong to a specific content version. Reject unknown, repeated,
/// or overlapping fragments rather than silently producing a different clipboard.
#[derive(Debug, thiserror::Error)]
pub enum TextSliceError {
    #[error("too many text slices selected")]
    TooMany,
    #[error("text slice selection contains duplicates")]
    Duplicate,
    #[error("text slices are stale; reopen the preview")]
    Stale,
    #[error("selected text slices overlap")]
    Overlap,
}

pub fn selected_slice_text(
    model: &TextSliceModel,
    ids: &[String],
) -> Result<String, TextSliceError> {
    if ids.len() > LIMIT {
        return Err(TextSliceError::TooMany);
    }
    let wanted: HashSet<_> = ids.iter().collect();
    if wanted.len() != ids.len() {
        return Err(TextSliceError::Duplicate);
    }
    let mut slices = model
        .levels
        .values()
        .flatten()
        .filter(|s| wanted.contains(&s.id))
        .collect::<Vec<_>>();
    if slices.len() != ids.len() {
        return Err(TextSliceError::Stale);
    }
    slices.sort_by_key(|s| (s.start, s.end));
    let wanted: HashSet<usize> = slices.iter().flat_map(|s| [s.start, s.end]).collect();
    let max_offset = wanted.iter().copied().max().unwrap_or(0);
    let mut byte_offsets = HashMap::with_capacity(wanted.len());
    let mut units = 0;
    for (index, ch) in model.text.char_indices() {
        if wanted.contains(&units) {
            byte_offsets.insert(units, index);
        }
        if units >= max_offset {
            break;
        }
        units += ch.len_utf16();
    }
    if !byte_offsets.contains_key(&units) && units == max_offset {
        byte_offsets.insert(units, model.text.len());
    }
    let mut result = String::new();
    let mut previous = None;
    for slice in slices {
        if let Some(end) = previous {
            if slice.start < end {
                return Err(TextSliceError::Overlap);
            }
            let gap = &model.text[byte_offsets[&end]..byte_offsets[&slice.start]];
            if GAP.is_match(gap) {
                result.push_str(gap);
            } else {
                result.push('\n');
            }
        }
        result.push_str(&slice.text);
        previous = Some(slice.end);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids(model: &TextSliceModel, level: &str, texts: &[&str]) -> Vec<String> {
        texts
            .iter()
            .map(|t| {
                model.levels[level]
                    .iter()
                    .find(|s| s.text == *t)
                    .unwrap()
                    .id
                    .clone()
            })
            .collect()
    }
    #[test]
    fn multilingual_offsets_preserve_source() {
        for source in [
            "王小明 will join المشروع غدًا。พรุ่งนี้พบกันที่กรุงเทพฯ 👋",
            "请添加适量动画，不影响性能。",
            "👩🏽‍💻 Alpha e\u{301} beta",
        ] {
            let model = build_text_slice_model(source);
            let units: Vec<_> = source.encode_utf16().collect();
            for slices in model.levels.values() {
                for s in slices {
                    assert_eq!(String::from_utf16(&units[s.start..s.end]).unwrap(), s.text);
                }
            }
            assert!(!model.levels["word"].is_empty());
        }
    }
    #[test]
    fn structured_fields_preserve_identifiers() {
        let m = build_text_slice_model(
            "收件人：王小明\nDue date: Friday, 5 PM\nusername=alice.smith\npassword=P@ssw0rd!",
        );
        ids(
            &m,
            "phrase",
            &["王小明", "Friday", "5 PM", "alice.smith", "P@ssw0rd!"],
        );
        ids(&m, "word", &["alice.smith", "P@ssw0rd!"]);
    }
    #[test]
    fn quotes_are_not_split_or_overlapped() {
        let m =
            build_text_slice_model("git commit -m \"feat: improve preview\"\ngit push origin main");
        assert_eq!(
            m.levels["phrase"]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            [
                "git commit -m \"feat: improve preview\"",
                "git push origin main"
            ]
        );
    }
    #[test]
    fn joins_source_order_and_preserves_only_adjacent_punctuation() {
        let m = build_text_slice_model("Alpha, beta and gamma");
        assert_eq!(
            selected_slice_text(&m, &ids(&m, "word", &["beta", "Alpha"])).unwrap(),
            "Alpha, beta"
        );
        assert_eq!(
            selected_slice_text(&m, &ids(&m, "word", &["Alpha", "gamma"])).unwrap(),
            "Alpha\ngamma"
        );
        let m = build_text_slice_model("Recipient: Alice\nProject: Aurora\nDue: Friday");
        assert_eq!(
            selected_slice_text(&m, &ids(&m, "phrase", &["Friday", "Alice", "Aurora"])).unwrap(),
            "Alice\nAurora\nFriday"
        );
    }
    #[test]
    fn rejects_invalid_and_overlapping_selections() {
        let m = build_text_slice_model("Alice Johnson");
        assert!(selected_slice_text(&m, &["unknown".into()]).is_err());
        let mut selected = ids(&m, "word", &["Alice"]);
        selected.extend(ids(&m, "phrase", &["Alice Johnson"]));
        assert!(selected_slice_text(&m, &selected).is_err());
    }
    #[test]
    fn limit_never_splits_surrogates_or_combining_sequences() {
        let source = format!("{}👋e\u{301}", "x".repeat(MAX_UTF16 - 1));
        let m = build_text_slice_model(&source);
        assert!(m.truncated);
        assert!(m.levels["block"].iter().all(|s| !s.text.ends_with('�')));
        assert_eq!(m.levels["document"][0].text, source);
    }
}
