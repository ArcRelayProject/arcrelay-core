use super::super::*;

// Original coverage.
#[test]
fn test_detect_markdown() {
    let md_text = r#"# Title
## Subtitle
- Item 1
- Item 2
Some **bold** text"#;
    assert!(matches!(
        TextDetector::detect(md_text),
        ClipboardTextSyntax::Markdown
    ));
}

#[test]
fn test_detect_rust_code() {
    let rust_code = r#"fn main() {
    println!("Hello, world!");
}"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(rust_code) {
        assert_eq!(language, Some("rust".to_string()));
    } else {
        panic!("Should detect as Rust code");
    }
}

#[test]
fn test_detect_plain() {
    let plain_text = "This is just some plain text.";
    assert!(matches!(
        TextDetector::detect(plain_text),
        ClipboardTextSyntax::Plain
    ));
}

#[test]
fn test_detect_xml_not_svg_or_html() {
    let xml_text = r#"<?xml version="1.0" encoding="UTF-8"?>
<note>
    <to>Tove</to>
    <from>Jani</from>
</note>"#;
    assert!(matches!(
        TextDetector::detect(xml_text),
        ClipboardTextSyntax::Xml
    ));
}

#[test]
fn test_detect_url_variations() {
    assert!(matches!(
        TextDetector::detect("https://www.example.com/search?q=test"),
        ClipboardTextSyntax::Url
    ));
    assert!(matches!(
        TextDetector::detect("www.google.com"),
        ClipboardTextSyntax::Url
    ));
    assert!(matches!(
        TextDetector::detect("example.com/path"),
        ClipboardTextSyntax::Url
    ));
    assert!(matches!(
        TextDetector::detect("example.com"),
        ClipboardTextSyntax::Plain
    ));
    assert!(matches!(
        TextDetector::detect("https://example.com\nline2"),
        ClipboardTextSyntax::Plain
    ));
}

// Extended regression coverage.

#[test]
fn test_detect_html_as_code() {
    let html_code = r#"
<!doctype html>
<html>
    <head><title>Test</title></head>
    <body><h1>Hello</h1></body>
</html>
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(html_code) {
        assert_eq!(language, Some("html".to_string()));
    } else {
        panic!("Should detect as HTML code");
    }
}

#[test]
fn test_detect_css_as_code() {
    let css_code = r#"
body {
    font-family: "Arial", sans-serif;
    color: #333;
}
a:hover {
    text-decoration: underline;
}
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(css_code) {
        assert_eq!(language, Some("css".to_string()));
    } else {
        panic!("Should detect as CSS code");
    }
}

#[test]
fn test_detect_markdown_with_table() {
    let md_with_table = r#"
# Report
| Header 1 | Header 2 |
|----------|----------|
| Cell 1   | Cell 2   |
"#;
    assert!(matches!(
        TextDetector::detect(md_with_table),
        ClipboardTextSyntax::Markdown
    ));
}

#[test]
fn test_detect_valid_svg() {
    // Complete SVG document.
    let valid_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
    <circle cx="50" cy="50" r="40" fill="red"/>
</svg>"#;
    assert!(matches!(
        TextDetector::detect(valid_svg),
        ClipboardTextSyntax::Svg
    ));

    // SVG with an XML declaration.
    let svg_with_declaration = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
    <rect x="10" y="10" width="80" height="80"/>
</svg>"#;
    assert!(matches!(
        TextDetector::detect(svg_with_declaration),
        ClipboardTextSyntax::Svg
    ));
}

#[test]
fn test_detect_invalid_svg() {
    // Missing closing tag.
    let no_closing = r#"<svg xmlns="http://www.w3.org/2000/svg">"#;
    assert!(!matches!(
        TextDetector::detect(no_closing),
        ClipboardTextSyntax::Svg
    ));

    // Missing namespace.
    let no_namespace = r#"<svg><circle cx="50" cy="50" r="40"/></svg>"#;
    assert!(!matches!(
        TextDetector::detect(no_namespace),
        ClipboardTextSyntax::Svg
    ));

    // SVG appears in the middle instead of at the start.
    let svg_in_code = r#"
const svg = '<svg xmlns="http://www.w3.org/2000/svg"></svg>';
console.log(svg);
"#;
    assert!(!matches!(
        TextDetector::detect(svg_in_code),
        ClipboardTextSyntax::Svg
    ));

    // `<svg` is immediately followed by another character, so it is not a valid tag.
    let invalid_tag = r#"<svgfoo xmlns="http://www.w3.org/2000/svg"></svgfoo>"#;
    assert!(!matches!(
        TextDetector::detect(invalid_tag),
        ClipboardTextSyntax::Svg
    ));

    // Contains the `<svg` text without an actual document structure.
    let svg_text = "This text mentions <svg> tag but is not svg";
    assert!(!matches!(
        TextDetector::detect(svg_text),
        ClipboardTextSyntax::Svg
    ));
}

#[test]
fn test_c_header_not_as_markdown() {
    let c_header = r#"
#ifndef MY_HEADER_H
#define MY_HEADER_H

void my_function();

#endif // MY_HEADER_H
"#;
    // Multiple lines begin with `#`, but the content is not Markdown.
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(c_header) {
        assert_eq!(language, Some("c".to_string()));
    } else {
        panic!("Should detect as C code, not Markdown");
    }
}

#[test]
fn test_detect_python_code() {
    let py_code = r#"
def hello(name):
    print(f"Hello, {name}")

# This is a comment
class MyClass:
    pass
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(py_code) {
        assert_eq!(language, Some("python".to_string()));
    } else {
        panic!("Should detect as Python code");
    }
}

#[test]
fn test_detect_javascript_code() {
    let js_code = r#"
const add = (a, b) => {
    return a + b;
};
// comment
console.log("Hello");
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(js_code) {
        assert_eq!(language, Some("javascript".to_string()));
    } else {
        panic!("Should detect as JavaScript code");
    }
}

#[test]
fn test_detect_typescript_code() {
    let ts_code = r#"
interface User {
    name: string;
}
const id: number = 10;
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(ts_code) {
        assert_eq!(language, Some("typescript".to_string()));
    } else {
        panic!("Should detect as TypeScript code");
    }
}

#[test]
fn test_detect_go_code() {
    let go_code = r#"
package main
import "fmt"

func main() {
    fmt.Println("Hello, Go")
}
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(go_code) {
        assert_eq!(language, Some("go".to_string()));
    } else {
        panic!("Should detect as Go code");
    }
}

#[test]
fn test_detect_cpp_code() {
    let cpp_code = r#"
#include <iostream>

int main() {
    std::cout << "Hello, C++" << std::endl;
    return 0;
}
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(cpp_code) {
        assert_eq!(language, Some("cpp".to_string()));
    } else {
        panic!("Should detect as C++ code");
    }
}

#[test]
fn test_detect_csharp_code() {
    let cs_code = r#"
using System;
namespace HelloWorld {
    class Program {
        static void Main(string[] args) {
            Console.WriteLine("Hello, C#");
        }
    }
}
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(cs_code) {
        assert_eq!(language, Some("csharp".to_string()));
    } else {
        panic!("Should detect as C# code");
    }
}

#[test]
fn test_detect_shell_script() {
    let shell_code = r#"
#!/bin/bash
echo "Hello Shell"
ls -la
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(shell_code) {
        assert_eq!(language, Some("bash".to_string()));
    } else {
        panic!("Should detect as Shell code");
    }
}

#[test]
fn test_detect_sql() {
    let sql_code = r#"
SELECT id, name FROM users WHERE age > 18;
INSERT INTO products (name, price) VALUES ('Apple', 1.50);
"#;
    if let ClipboardTextSyntax::Code { language } = TextDetector::detect(sql_code) {
        assert_eq!(language, Some("sql".to_string()));
    } else {
        panic!("Should detect as SQL code");
    }
}

#[test]
fn test_plain_text_not_as_code() {
    let plain_text = "This is a plain text. It might talk about public or class but it is not code. There are no { } or ; symbols in sequence.";
    assert!(matches!(
        TextDetector::detect(plain_text),
        ClipboardTextSyntax::Plain
    ));
}

#[test]
fn test_detect_onedev_yaml() {
    let onedev_yaml = r#"version: 34
jobs:
- name: cargo test and build
  jobExecutor: home-docker
  steps:
  - !CheckoutStep
    name: checkout
    cloneCredential: !DefaultCredential {}
    withLfs: false
    withSubmodules: false
    condition: ALL_PREVIOUS_STEPS_WERE_SUCCESSFUL
  - !CommandStep
    name: cargo build
    runInContainer: true
    image: rust:latest
    interpreter: !DefaultInterpreter
      commands: |
        apt-get update
        cargo build --release
"#;
    assert!(matches!(
        TextDetector::detect(onedev_yaml),
        ClipboardTextSyntax::Yaml
    ));
}

#[test]
fn test_detect_simple_yaml() {
    let simple_yaml = r#"name: John
age: 30
skills:
  - Rust
  - Python
address:
  city: New York
  zip: 10001
"#;
    assert!(matches!(
        TextDetector::detect(simple_yaml),
        ClipboardTextSyntax::Yaml
    ));
}

#[test]
fn test_debug_onedev_detection() {
    let onedev_yaml = r#"version: 34
jobs:
- name: cargo test and build
  jobExecutor: home-docker
  steps:
  - !CheckoutStep
    name: checkout
    cloneCredential: !DefaultCredential {}
"#;
    let result = TextDetector::detect(onedev_yaml);
    println!("Detection result: {:?}", result);

    // Exercise each detector directly.
    println!("is_json: {}", TextDetector::is_json(onedev_yaml));
    println!("is_yaml: {}", TextDetector::is_yaml(onedev_yaml));
    println!("is_markdown: {}", TextDetector::is_markdown(onedev_yaml));
    println!(
        "has_code_indicators: {}",
        TextDetector::has_code_indicators(onedev_yaml)
    );

    assert!(matches!(result, ClipboardTextSyntax::Yaml));
}
