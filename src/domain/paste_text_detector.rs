//! Text content type detection utilities.
//! Detects specific text formats such as JSON, Markdown, and SVG.

use crate::domain::clipboard::ClipboardTextSyntax;

/// Detects the format of text content.
pub struct TextDetector;

impl TextDetector {
    /// Detects the type of text content.
    ///
    /// # Detection priority
    /// 1. JSON (strict matching)
    /// 2. YAML (strict validation and serializability)
    /// 3. SVG
    /// 4. XML (precise root-element matching)
    /// 5. URL
    /// 6. Mermaid diagram (distinct keywords take precedence over code detection)
    /// 7. Code snippet (high threshold and clear language features)
    /// 8. Markdown (high threshold with strongly weighted features)
    /// 9. Plain text (default)
    pub fn detect(text: &str) -> ClipboardTextSyntax {
        let trimmed = text.trim();

        // Empty text.
        if trimmed.is_empty() {
            return ClipboardTextSyntax::Plain;
        }

        // 1. Detect JSON using strict conditions.
        if Self::is_json(trimmed) {
            return ClipboardTextSyntax::Json;
        }

        // 2. Detect magnet links early because their signature is unambiguous.
        if Self::is_magnet_link(trimmed) {
            return ClipboardTextSyntax::MagnetLink;
        }

        // 3. Detect JWTs, which have a distinctive structure.
        if Self::is_jwt_token(trimmed) {
            return ClipboardTextSyntax::JwtToken;
        }

        // 4. Detect colors (hex, RGB, or HSL).
        if Self::is_color(trimmed) {
            return ClipboardTextSyntax::Color;
        }

        // 5. Detect IP addresses.
        if Self::is_ip_address(trimmed) {
            return ClipboardTextSyntax::IpAddress;
        }

        // 6. Detect mxGraph before SVG and XML because it is a specialized XML format.
        if Self::is_mxgraph(trimmed) {
            return ClipboardTextSyntax::MxGraph;
        }

        // 7. Detect SVG.
        if Self::is_svg(trimmed) {
            return ClipboardTextSyntax::Svg;
        }

        // 8. Detect XML with stricter structure checks.
        if Self::is_xml(trimmed) {
            return ClipboardTextSyntax::Xml;
        }

        // 9. Detect Mermaid before code because Mermaid has distinctive keywords.
        if Self::is_mermaid(trimmed) {
            return ClipboardTextSyntax::Mermaid;
        }

        // 10. Detect code snippets before YAML and Markdown.
        if let Some(language) = Self::detect_code_language(trimmed) {
            // Prefer valid YAML over code with an unknown language because YAML often
            // contains code-like punctuation and indentation.
            if language == "unknown" && Self::is_yaml(trimmed) {
                return ClipboardTextSyntax::Yaml;
            }

            return ClipboardTextSyntax::Code {
                language: Some(language),
            };
        }

        // 11. Detect YAML using strict validation.
        if Self::is_yaml(trimmed) {
            return ClipboardTextSyntax::Yaml;
        }

        // 12. Detect a valid single-line URL.
        if Self::is_url(trimmed) {
            return ClipboardTextSyntax::Url;
        }

        // 13. Detect email addresses.
        if Self::is_email(trimmed) {
            return ClipboardTextSyntax::Email;
        }

        // 14. Detect phone numbers.
        if Self::is_phone_number(trimmed) {
            return ClipboardTextSyntax::PhoneNumber;
        }

        // 15. Detect file paths after URLs and email addresses to avoid false positives.
        if Self::is_file_path(trimmed) {
            return ClipboardTextSyntax::FilePath;
        }

        // 16. Detect Markdown using strict conditions.
        if Self::is_markdown(trimmed) {
            return ClipboardTextSyntax::Markdown;
        }

        // 17. Fall back to plain text.
        ClipboardTextSyntax::Plain
    }

    /// Returns whether the text is JSON.
    fn is_json(text: &str) -> bool {
        // Requiring matching outer delimiters provides a cheap initial filter.
        if (text.starts_with('{') && text.ends_with('}'))
            || (text.starts_with('[') && text.ends_with(']'))
        {
            // Run the more expensive JSON parser only after the delimiters match.
            return serde_json::from_str::<serde_json::Value>(text).is_ok();
        }
        false
    }

    /// Returns whether the text is YAML using focused validation.
    ///
    /// # Validation strategy
    /// 1. `serde_yaml` must parse it successfully.
    /// 2. The value must be a mapping or sequence.
    /// 3. Single-line text must use flow style (`{` or `[`) to avoid matching prose.
    fn is_yaml(text: &str) -> bool {
        // Attempt to parse YAML.
        if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(text) {
            // A single-line value must use flow style; multi-line structures are allowed.
            if text.lines().count() == 1 {
                let trimmed = text.trim();
                if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                    return false;
                }
            }

            // Accept only mappings and sequences.
            matches!(
                value,
                serde_yaml::Value::Mapping(_)
                    | serde_yaml::Value::Sequence(_)
                    | serde_yaml::Value::Tagged(_)
            )
        } else {
            false
        }
    }

    /// Returns whether the text is an mxGraph document using high-precision checks.
    ///
    /// mxGraph is an XML format for flowcharts, relationship diagrams, and similar graphs.
    /// Its core features are:
    /// 1. An `<mxCell` element.
    /// 2. Attributes such as `id=`, `value=`, and `style=`.
    /// 3. A `vertex=` or `edge=` attribute.
    /// 4. An optional `<mxGeometry` element.
    /// 5. An XML-based structure.
    fn is_mxgraph(text: &str) -> bool {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // 1. Require the core `<mxCell` element.
        if !lower.contains("<mxcell") {
            return false;
        }

        // 2. Validate the mxCell tag.
        if let Some(pos) = lower.find("<mxcell") {
            let after_tag = &lower[pos + 7..];
            // The tag name must be followed by whitespace, `>`, or an attribute.
            if !after_tag.is_empty()
                && !after_tag.starts_with('>')
                && !after_tag.starts_with(' ')
                && !after_tag.starts_with('\n')
                && !after_tag.starts_with('\t')
            {
                return false;
            }
        }

        // 3. Require the mxGraph-specific `vertex` or `edge` attribute.
        let has_vertex_or_edge = lower.contains("vertex=") || lower.contains("edge=");
        if !has_vertex_or_edge {
            return false;
        }

        // 4. An mxGeometry tag increases confidence but is not required.
        let has_geometry = lower.contains("<mxgeometry");

        // 5. Check for common mxGraph attributes.
        let has_common_attrs =
            lower.contains("id=") && (lower.contains("style=") || lower.contains("value="));

        // Require mxCell and a vertex or edge attribute. Geometry or another
        // common attribute raises confidence further.
        has_vertex_or_edge && (has_geometry || has_common_attrs)
    }

    /// Returns whether the text is SVG using high-precision checks.
    fn is_svg(text: &str) -> bool {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // 1. Require a closed `<svg>...</svg>` element pair.
        if !lower.contains("</svg>") {
            return false;
        }

        // 2. Find the first valid `<svg` tag.
        let svg_tag_pos = if let Some(pos) = lower.find("<svg") {
            // The tag name must be followed by whitespace, `>`, or the end of the tag.
            let after_svg = &lower[pos + 4..];
            if after_svg.is_empty()
                || after_svg.starts_with('>')
                || after_svg.starts_with(' ')
                || after_svg.starts_with('\n')
                || after_svg.starts_with('\t')
            {
                pos
            } else {
                // Reject prefixes such as `<svgfoo>` that are not actual SVG tags.
                return false;
            }
        } else {
            return false;
        };

        // 3. Require `<svg` at the document start or after only XML declarations/comments.
        let before_svg = &lower[..svg_tag_pos].trim();
        if !before_svg.is_empty() {
            // Only an XML declaration or comment may precede the root element.
            if !before_svg.starts_with("<?xml") && !before_svg.starts_with("<!--") {
                return false;
            }
        }

        // 4. Require the SVG namespace, a strong signal of valid SVG.
        if !lower.contains("http://www.w3.org/2000/svg") {
            return false;
        }

        // 5. Ensure `</svg>` follows `<svg`.
        if let Some(close_pos) = lower.find("</svg>") {
            if close_pos <= svg_tag_pos {
                return false;
            }
        }

        true
    }

    /// Returns whether the text is XML using high-precision checks.
    fn is_xml(text: &str) -> bool {
        let trimmed = text.trim();
        // An XML declaration is a strong indicator of XML content.
        if trimmed.starts_with("<?xml") {
            return true;
        }

        if trimmed.starts_with('<') && trimmed.ends_with('>') {
            let lower = text.to_lowercase();
            // Exclude more specific XML-based formats.
            if lower.contains("<svg")
                || lower.contains("<!doctype html>")
                || lower.contains("<html")
            {
                return false;
            }

            // Require a closed root element such as `<root>...</root>` to avoid
            // misclassifying ordinary text or HTML fragments as XML.
            if let Some(first_tag_end) = trimmed.find(|c: char| c == '>' || c.is_whitespace()) {
                let tag_name = &trimmed[1..first_tag_end];
                // Ensure the tag name is valid and is not a closing tag.
                if !tag_name.is_empty() && !tag_name.starts_with('/') {
                    let closing_tag = format!("</{}>", tag_name);
                    if trimmed.ends_with(&closing_tag) {
                        return true;
                    }
                }
            }

            // For compact XML fragments such as `<data><value/></data>`, fall back
            // to checking balanced angle brackets.
            let open_count = trimmed.matches('<').count();
            let close_count = trimmed.matches('>').count();
            // Require at least one closing tag and balanced angle brackets.
            return open_count >= 2 && open_count == close_count;
        }

        false
    }

    /// Returns whether the text is a URL.
    fn is_url(text: &str) -> bool {
        // URLs must fit on one line.
        if text.lines().count() != 1 {
            return false;
        }

        // URLs cannot contain unencoded spaces.
        if text.contains(' ') {
            return false;
        }

        // Check common schemes.
        let lower = text.to_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("ftp://")
            || lower.starts_with("file://")
        {
            return text.len() > 10; // `https://a.b` is the shortest accepted form.
        }

        // Check the `www.example.com` form.
        if lower.starts_with("www.") {
            return text.matches('.').count() >= 2; // Require at least `www.domain.com`.
        }

        // Check the `example.com/path` form, which requires both a TLD and a path.
        if !lower.contains('.') || !lower.contains('/') {
            return false;
        }

        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() < 2 {
            return false;
        }
        // Require a TLD of at least two characters.
        if let Some(domain_part) = text.split('/').next() {
            if let Some(tld) = domain_part.split('.').next_back() {
                if tld.len() >= 2 {
                    return true;
                }
            }
        }

        false
    }

    /// Returns whether the text is an email address.
    fn is_email(text: &str) -> bool {
        // Email addresses must fit on one line.
        if text.lines().count() != 1 {
            return false;
        }

        let trimmed = text.trim();
        // Enforce the length limit.
        if trimmed.len() < 5 || trimmed.len() > 254 {
            return false;
        }

        // Require `@`.
        let parts: Vec<&str> = trimmed.split('@').collect();
        if parts.len() != 2 {
            return false;
        }

        let local = parts[0];
        let domain = parts[1];

        if local.is_empty() || domain.is_empty() {
            return false;
        }

        // The domain must contain a dot that is neither first nor last.
        if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
            return false;
        }

        // The local part allows letters, digits, dots, underscores, plus signs, and hyphens.
        let is_valid_char =
            |c: char| c.is_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '+';

        if !local.chars().all(is_valid_char) {
            return false;
        }

        // The domain allows letters, digits, dots, and hyphens.
        let is_valid_domain_char = |c: char| c.is_alphanumeric() || c == '.' || c == '-';

        if !domain.chars().all(is_valid_domain_char) {
            return false;
        }

        true
    }

    /// Returns whether the text is a phone number.
    fn is_phone_number(text: &str) -> bool {
        // Phone numbers must fit on one line.
        if text.lines().count() != 1 {
            return false;
        }

        let trimmed = text.trim();
        // Account for international prefixes and formatting while limiting length.
        if trimmed.len() < 7 || trimmed.len() > 20 {
            return false;
        }

        // Allow digits, spaces, plus signs, hyphens, and parentheses.
        let valid_chars = [' ', '+', '-', '(', ')'];

        // Require at least one digit.
        let digit_count = trimmed.chars().filter(|c| c.is_numeric()).count();
        if !(7..=15).contains(&digit_count) {
            return false;
        }

        // Reject any unsupported character.
        if !trimmed
            .chars()
            .all(|c| c.is_numeric() || valid_chars.contains(&c))
        {
            return false;
        }

        // If present, `+` must be the first character.
        if let Some(idx) = trimmed.find('+') {
            if idx != 0 {
                return false;
            }
        }

        true
    }

    /// Returns whether the text is a magnet link.
    fn is_magnet_link(text: &str) -> bool {
        text.trim().starts_with("magnet:?")
    }

    /// Returns whether the text is a JWT.
    fn is_jwt_token(text: &str) -> bool {
        let trimmed = text.trim();
        // JWTs commonly start with `ey`.
        if !trimmed.starts_with("ey") {
            return false;
        }
        // Require two dots: `Header.Payload.Signature`.
        let parts: Vec<&str> = trimmed.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        // Check that each Base64URL segment contains only letters, digits, `-`, and `_`.
        let is_base64url = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        };

        is_base64url(parts[0]) && is_base64url(parts[1]) && is_base64url(parts[2])
    }

    /// Returns whether the text is a color value.
    fn is_color(text: &str) -> bool {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // Hex Color: #RRGGBB or #RGB or #RRGGBBAA
        if let Some(hex_part) = trimmed.strip_prefix('#') {
            let len = hex_part.len();
            if len == 3 || len == 4 || len == 6 || len == 8 {
                return hex_part.chars().all(|c| c.is_ascii_hexdigit());
            }
            return false;
        }

        // RGB/RGBA: rgb(r, g, b) / rgba(r, g, b, a)
        if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
            return lower.ends_with(')');
        }

        // HSL/HSLA: hsl(h, s, l) / hsla(h, s, l, a)
        if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
            return lower.ends_with(')');
        }

        false
    }

    /// Returns whether the text is an IP address.
    fn is_ip_address(text: &str) -> bool {
        let trimmed = text.trim();
        // IPv4
        if trimmed.contains('.') {
            // IPv4 has four numeric components in the range 0–255.
            let parts: Vec<&str> = trimmed.split('.').collect();
            if parts.len() != 4 {
                return false;
            }
            for part in parts {
                if part.is_empty() {
                    return false;
                }
                if part.parse::<u8>().is_err() {
                    return false;
                }
            }
            return true;
        }

        // IPv6
        if trimmed.contains(':') {
            // IPv6 contains at least two colons and only hexadecimal digits or colons.
            if trimmed.matches(':').count() < 2 {
                return false;
            }
            return trimmed.chars().all(|c| c.is_ascii_hexdigit() || c == ':');
        }

        false
    }

    /// Returns whether the text is a file path.
    fn is_file_path(text: &str) -> bool {
        let trimmed = text.trim();
        // File paths must fit on one line.
        if text.lines().count() != 1 {
            return false;
        }

        // Enforce the length limit.
        if trimmed.len() < 2 || trimmed.len() > 260 {
            return false;
        }

        // Windows absolute path: `C:\...` or `C:/...`.
        if trimmed.len() >= 3
            && trimmed
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && trimmed.chars().nth(1) == Some(':')
            && (trimmed.chars().nth(2) == Some('\\') || trimmed.chars().nth(2) == Some('/'))
        {
            // Reject invalid path characters.
            let invalid_chars = ['<', '>', '"', '|', '?', '*'];
            return !trimmed.chars().any(|c| invalid_chars.contains(&c));
        }

        // Windows UNC path: `\\Server\Share`.
        if trimmed.starts_with("\\\\") {
            let invalid_chars = ['<', '>', '"', '|', '?', '*'];
            return !trimmed.chars().any(|c| invalid_chars.contains(&c));
        }

        // Unix absolute path: `/...`.
        if trimmed.starts_with('/') {
            // Exclude `//`, which may be a comment or scheme-relative URL.
            if trimmed.starts_with("//") {
                return false;
            }
            // Reject invalid path characters.
            return !trimmed.contains('\0');
        }

        // Home-directory path: `~/...`.
        if trimmed.starts_with("~/") {
            return true;
        }

        false
    }

    /// Returns whether the text is a Mermaid diagram.
    ///
    /// Mermaid is a Markdown extension for flowcharts, sequence diagrams, Gantt charts,
    /// and similar diagrams. Its core features are:
    /// 1. An optional ` ```mermaid ` code fence.
    /// 2. A diagram keyword such as `graph`, `sequenceDiagram`, `classDiagram`,
    ///    `stateDiagram`, `erDiagram`, `gantt`, `pie`, `journey`, or `gitGraph`.
    /// 3. Mermaid-specific arrows such as `-->`, `--->`, `===>`, or `-.->`.
    /// 4. Node and edge definitions.
    fn is_mermaid(text: &str) -> bool {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // 1. Check for a Markdown code fence.
        let is_in_code_block = lower.starts_with("```mermaid") || lower.contains("\n```mermaid");

        // Extract the diagram content without the fence markers.
        let content = if is_in_code_block {
            if let Some(start_pos) = lower.find("```mermaid") {
                let after_marker = &trimmed[start_pos + 10..]; // 10 = len("```mermaid")
                if let Some(end_pos) = after_marker.find("```") {
                    &after_marker[..end_pos]
                } else {
                    after_marker
                }
            } else {
                trimmed
            }
        } else {
            trimmed
        };

        let content_lower = content.to_lowercase();
        let lines: Vec<&str> = content.lines().collect();

        // 2. Detect a Mermaid diagram keyword at the start of a trimmed line.
        let mermaid_chart_types = [
            "graph ",
            "graph\n",
            "graph\r",
            "flowchart ",
            "flowchart\n",
            "flowchart\r",
            "sequencediagram",
            "classdiagram",
            "statediagram",
            "erdiagram",
            "gantt",
            "pie ",
            "pie\n",
            "pie\r",
            "journey",
            "gitgraph",
            "requirementdiagram",
            "c4context",
            "mindmap",
            "timeline",
            "zenuml",
            "sankey",
        ];

        let mut has_chart_type = false;
        for line in &lines {
            let line_lower = line.trim().to_lowercase();
            if line_lower.is_empty() {
                continue;
            }
            for &chart_type in &mermaid_chart_types {
                if line_lower.starts_with(chart_type) || line_lower == chart_type.trim() {
                    has_chart_type = true;
                    break;
                }
            }
            if has_chart_type {
                break;
            }
        }

        // Without a diagram keyword, the text is unlikely to be Mermaid.
        if !has_chart_type {
            return false;
        }

        // 3. Detect Mermaid-specific arrow syntax.
        let mermaid_arrows = [
            "-->", "--->", "====>", "-.->", "==>", "--", "---", "====", "-.-", "===", "...", "->>",
            "-->>",
        ];

        let has_arrows = mermaid_arrows
            .iter()
            .any(|&arrow| content_lower.contains(arrow));

        // 4. Detect Mermaid node patterns such as `A[Label]`, `B(Label)`,
        // `C{Label}`, `D((Label))`, `E>Label]`, and `F[[Label]]`.
        let has_node_syntax = content.contains('[') && content.contains(']')
            || content.contains('(') && content.contains(')')
            || content.contains('{') && content.contains('}')
            || content.contains(">>");

        // 5. Detect the `participant` keyword used by sequence diagrams.
        let has_participant = content_lower.contains("participant ");

        // 6. Detect other common Mermaid keywords.
        let mermaid_keywords = [
            "subgraph",
            "end",
            "note ",
            "loop",
            "alt",
            "opt",
            "par",
            "activate",
            "deactivate",
            "title ",
            "section",
            "class ",
            "style ",
            "linkstyle",
            "classdef",
            "branch ",
            "checkout",
            "commit",
            "merge",
            "dateformat",
        ];

        let keyword_count = mermaid_keywords
            .iter()
            .filter(|&&kw| content_lower.contains(kw))
            .count();

        // A diagram keyword is mandatory, along with at least one strong signal:
        // a code fence, arrow or node syntax, `participant`, or another Mermaid keyword.
        has_chart_type
            && (is_in_code_block
                || has_arrows
                || has_node_syntax
                || has_participant
                || keyword_count >= 1)
    }

    fn is_markdown(text: &str) -> bool {
        let lines: Vec<&str> = text.lines().collect();

        if lines.is_empty() {
            return false;
        }

        let mut markdown_indicators = 0;

        for line in lines.iter().take(20) {
            // Inspect only the first 20 lines to bound detection cost.
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }

            if trimmed.starts_with("# ")
                || trimmed.starts_with("## ")
                || trimmed.starts_with("### ")
            {
                markdown_indicators += 2;
            }
            if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
                markdown_indicators += 1;
            }
            if trimmed.len() > 2 {
                let mut chars = trimmed.chars();
                if chars.next().is_some_and(|c| c.is_numeric())
                    && chars.next() == Some('.')
                    && chars.next() == Some(' ')
                {
                    markdown_indicators += 1;
                }
            }
            if trimmed.starts_with("> ") {
                markdown_indicators += 1;
            }
            // Code fences and tables are strong Markdown signals, so weight them highly.
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                markdown_indicators += 4;
            }
            if trimmed.len() >= 3
                && (trimmed.starts_with("---")
                    || trimmed.starts_with("***")
                    || trimmed.starts_with("___"))
                && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_')
            {
                markdown_indicators += 2;
            }
            if trimmed.contains('|')
                && trimmed.contains('-')
                && trimmed.chars().filter(|&c| c == '|').count() >= 2
                && trimmed
                    .chars()
                    .all(|c| c.is_whitespace() || c == '|' || c == '-' || c == ':')
            {
                markdown_indicators += 4; // Tables are a strong signal.
            }
            if trimmed.contains("**") || trimmed.contains("__") || trimmed.contains('`') {
                markdown_indicators += 1;
            }
            if trimmed.contains("](") && trimmed.contains('[') {
                markdown_indicators += 2;
            }
        }

        // Use a high threshold to reduce false positives. Single-line text needs
        // a stronger combination of features.
        if lines.len() == 1 {
            return markdown_indicators >= 3; // Previously 2.
        }

        // Multi-line text needs at least four feature points.
        markdown_indicators >= 4 // Previously 3.
    }

    // Enhanced code-detection logic follows.
}

mod code;

#[cfg(test)]
mod tests {
    mod basics;
    mod formats;
    mod yaml_regressions;
}
