use super::*;

impl TextDetector {
    /// Detects the programming language of a code snippet.
    pub(super) fn detect_code_language(text: &str) -> Option<String> {
        // Prefer strong language-specific signals.
        if let Some(lang) = Self::guess_language(text) {
            // Return immediately for a high-confidence language match.
            match lang.as_str() {
                "html" | "css" | "php" | "sql" | "go" | "c" | "cpp" | "java" | "csharp"
                | "rust" | "python" | "ruby" | "javascript" | "typescript" | "swift" | "bash"
                | "shell" => {
                    return Some(lang);
                }
                _ => {
                    // Weak language signals require validation by general code indicators.
                }
            }
        }

        // High-precision mode does not classify unknown code from general indicators.
        // Prefer a false negative over a classification without language-specific evidence.
        None
    }

    /// Returns whether the text contains high-confidence code indicators.
    #[allow(dead_code)]
    pub(super) fn has_code_indicators(text: &str) -> bool {
        fn contains_word(hay: &str, needle: &str) -> bool {
            let mut start = 0usize;
            while let Some(pos) = hay[start..].find(needle) {
                let idx = start + pos;
                let before = hay[..idx].chars().next_back();
                let after = hay[idx + needle.len()..].chars().next();
                let before_ok = before.is_none_or(|c| !c.is_alphanumeric() && c != '_');
                let after_ok = after.is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if before_ok && after_ok {
                    return true;
                }
                start = idx + 1;
            }
            false
        }

        let mut indicators = 0;
        let lines = text.lines().count();
        let lower = text.to_lowercase();

        if text.starts_with("#!") {
            indicators += 3;
        }
        if contains_word(&lower, "select") && contains_word(&lower, "from") {
            return true;
        }
        if lower.starts_with("create table")
            || lower.starts_with("insert into")
            || lower.starts_with("update ")
        {
            return true;
        }

        // Treat C and C++ preprocessor directives as strong signals.
        let keywords = [
            "fn",
            "func",
            "function",
            "def",
            "class",
            "interface",
            "struct",
            "impl",
            "trait",
            "public",
            "private",
            "static",
            "import",
            "package",
            "namespace",
            "use",
            "#include",
            "#define",
            "#ifndef",
            "#ifdef",
            "const",
            "let",
            "var",
            "new",
            "return",
            "async",
            "await",
            "enum",
            "type",
            "throws",
        ];
        for keyword in &keywords {
            if contains_word(&lower, keyword) {
                indicators += 1;
            }
        }

        let symbols = ["=>", "->", "::", "===", "!==", "++", "--", "+=", "-="];
        for symbol in &symbols {
            if text.contains(symbol) {
                indicators += 1;
            }
        }

        if text.contains("//")
            || text.contains("/*")
            || text.lines().any(|l| l.trim_start().starts_with('#'))
        {
            indicators += 1;
        }

        if lines > 1 && text.contains(';') {
            let semi_count = text.matches(';').count();
            if semi_count > lines / 2 || semi_count > 3 {
                indicators += 2;
            } else {
                indicators += 1;
            }
        }

        if text.contains('(') && text.contains(')') {
            indicators += 1;
            if text.contains(';') {
                indicators += 1;
            }
        }

        if lines > 2 && (text.contains("\n    ") || text.contains("\n\t")) {
            indicators += 1;
        }

        let mut open_brace = 0usize;
        let mut close_brace = 0usize;
        let chars: Vec<char> = text.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == '{' {
                let prev_ws = i == 0 || chars[i - 1].is_whitespace();
                let next_ws = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
                if prev_ws && next_ws {
                    continue;
                }
                open_brace += 1;
            }
            if c == '}' {
                let prev_ws = i == 0 || chars[i - 1].is_whitespace();
                let next_ws = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
                if prev_ws && next_ws {
                    continue;
                }
                close_brace += 1;
            }
        }

        if open_brace > 0 && open_brace == close_brace {
            indicators += 2;
        } else if open_brace > 0 || close_brace > 0 {
            indicators += 1;
        }

        // A threshold of five requires stronger evidence and reduces false positives.
        indicators >= 5
    }

    /// Guesses the programming language using precise, language-specific features.
    fn guess_language(text: &str) -> Option<String> {
        let lower = text.to_lowercase();

        // Strong-signal detection (high priority).
        if lower.starts_with("<!doctype html>")
            || (lower.contains("<html")
                && lower.contains("</html>")
                && (lower.contains("<body") || lower.contains("<head")))
        {
            return Some("html".to_string());
        }

        // CSS detection is already precise.
        let js_ts_keywords = [
            "const",
            "let",
            "function",
            "interface",
            "import",
            "class",
            "type",
        ];
        let has_js_ts = js_ts_keywords.iter().any(|kw| lower.contains(kw));
        let brace_count = lower.matches('{').count();
        if !has_js_ts
            && brace_count > 0
            && lower.matches('}').count() == brace_count
            && lower.matches(':').count() >= brace_count
            && lower.matches(';').count() >= std::cmp::max(1, brace_count / 2)
        {
            let mut css_like_lines = 0;
            let total_lines = text.lines().count();
            if total_lines > 0 {
                for line in text.lines().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    if (line.ends_with('{') && !line.starts_with('@'))
                        || (line.contains(':') && line.ends_with(';') && !line.contains('='))
                    {
                        css_like_lines += 1;
                    }
                }
                if css_like_lines > total_lines / 2 {
                    return Some("css".to_string());
                }
            }
        }

        if text.starts_with("#!/bin/") || text.starts_with("#!/usr/bin/") {
            if text.contains("bash") {
                return Some("bash".to_string());
            }
            if text.contains("sh") {
                return Some("shell".to_string());
            }
            if text.contains("zsh") {
                return Some("zsh".to_string());
            }
            if text.contains("python") {
                return Some("python".to_string());
            }
            return Some("shell".to_string());
        }

        if text.contains("<?php") {
            return Some("php".to_string());
        }

        if text.contains("package main")
            || (text.contains("package ") && text.contains("func ") && text.contains("import ("))
        {
            return Some("go".to_string());
        }

        // Include directives such as `#ifndef` to identify C and C++ headers accurately.
        if text.contains("#include <") || text.contains("#define ") || text.contains("#ifndef ") {
            if text.contains("std::") || text.contains("namespace ") || text.contains("template <")
            {
                return Some("cpp".to_string());
            }
            return Some("c".to_string());
        }

        // Treat Java-specific keywords such as `extends` and `implements` as strong signals.
        if text.contains("import java.")
            || text.contains("public class ")
            || text.contains("System.out.println")
            || (text.contains(" class ")
                && (text.contains(" extends ") || text.contains(" implements ")))
        {
            return Some("java".to_string());
        }

        // Include the C#-specific `async Task` pattern.
        if text.contains("using System;")
            || (text.contains("namespace ")
                && text.contains("class ")
                && !text.contains("#include"))
            || text.contains(" async Task<")
        {
            return Some("csharp".to_string());
        }

        // Include Rust-specific patterns such as `match` and `let mut`.
        if text.contains("fn main()")
            || (text.contains("fn ")
                && (text.contains("->")
                    || text.contains("impl ")
                    || text.contains("::")
                    || text.contains("use std::")))
            || text.contains("let mut ")
            || (text.contains(" match ") && text.contains("=>"))
        {
            return Some("rust".to_string());
        }

        // Python requires a strong combination: `def name():` or `class name:` plus indentation.
        let has_python_def = text.lines().any(|l| {
            let trimmed = l.trim();
            (trimmed.starts_with("def ") && trimmed.contains('(') && trimmed.ends_with(':'))
                || (trimmed.starts_with("class ") && trimmed.ends_with(':'))
                || (trimmed.starts_with("async def ")
                    && trimmed.contains('(')
                    && trimmed.ends_with(':'))
        });
        let has_python_decorator = text.lines().any(|l| {
            let trimmed = l.trim();
            trimmed.starts_with('@') && !trimmed.contains(' ') && trimmed.len() > 1
        });
        let has_python_import = text.lines().any(|l| {
            let trimmed = l.trim();
            (trimmed.starts_with("import ") || trimmed.starts_with("from "))
                && !trimmed.contains('{')
                && !trimmed.contains("java.")
        });
        if has_python_def || (has_python_decorator && has_python_import) {
            return Some("python".to_string());
        }

        // Ruby also requires a strong combination of features.
        if (text.contains("def ") && text.contains("\nend") && !text.contains('{'))
            || (text.contains("class ") && text.contains("\nend") && !text.contains('{'))
            || (text.contains("require '") || text.contains("require \""))
        {
            return Some("ruby".to_string());
        }

        if (lower.contains("select ") && lower.contains("from "))
            || lower.starts_with("create table ")
            || lower.starts_with("insert into ")
            || lower.starts_with("update ")
        {
            return Some("sql".to_string());
        }

        // Weak-signal detection (low priority). In high-precision mode, weak signals
        // never produce a result unless several stronger conditions also match.

        // TypeScript requires both type annotations and a function or class definition.
        let has_ts_types = text.contains(": string")
            || text.contains(": number")
            || text.contains(": boolean")
            || text.contains("<T>")
            || text.contains("interface ")
            || text.contains("private readonly ");
        let has_ts_syntax = text.contains("const ")
            || text.contains("let ")
            || (text.contains("function") && text.contains('('))
            || text.contains("=>");
        if has_ts_types && has_ts_syntax && text.contains('{') && text.contains('}') {
            return Some("typescript".to_string());
        }

        // JavaScript requires structural code features; declarations alone are too ambiguous.
        let has_js_function =
            (text.contains("function ") && text.contains('(') && text.contains('{'))
                || (text.contains("=>") && text.contains('{'));
        let has_js_console = text.contains("console.log(") || text.contains("console.error(");
        let has_js_module = text.contains("module.exports") || text.contains("require(");
        if has_js_function || has_js_console || has_js_module {
            return Some("javascript".to_string());
        }

        // Swift requires an unambiguous Swift-specific feature.
        if text.contains("import UIKit")
            || text.contains("import Foundation")
            || text.contains("import SwiftUI")
            || ((text.contains("func ") && text.contains("->"))
                && (text.contains("var ") || text.contains("let "))
                && text.contains('{'))
        {
            return Some("swift".to_string());
        }

        None
    }
}
