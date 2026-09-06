use super::super::*;

#[test]
fn test_detect_email() {
    assert!(matches!(
        TextDetector::detect("test@example.com"),
        ClipboardTextSyntax::Email
    ));
    assert!(matches!(
        TextDetector::detect("user.name+tag@sub.domain.co.uk"),
        ClipboardTextSyntax::Email
    ));
    assert!(!matches!(
        TextDetector::detect("not an email"),
        ClipboardTextSyntax::Email
    ));
    assert!(!matches!(
        TextDetector::detect("user@domain"), // Missing TLD part or dot
        ClipboardTextSyntax::Email
    ));
}

#[test]
fn test_detect_phone_number() {
    assert!(matches!(
        TextDetector::detect("13800138000"),
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(matches!(
        TextDetector::detect("+86 138 0013 8000"),
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(matches!(
        TextDetector::detect("010-12345678"),
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(matches!(
        TextDetector::detect("(010) 12345678"),
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(!matches!(
        TextDetector::detect("12345"), // Too short
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(!matches!(
        TextDetector::detect("12345678901234567890"), // Too long
        ClipboardTextSyntax::PhoneNumber
    ));
    assert!(!matches!(
        TextDetector::detect("123 abc 456"), // Contains letters
        ClipboardTextSyntax::PhoneNumber
    ));
}

#[test]
fn test_detect_magnet_link() {
    assert!(matches!(
        TextDetector::detect("magnet:?xt=urn:btih:1234567890abcdef"),
        ClipboardTextSyntax::MagnetLink
    ));
    assert!(!matches!(
        TextDetector::detect("http://example.com/magnet"),
        ClipboardTextSyntax::MagnetLink
    ));
}

#[test]
fn test_detect_jwt_token() {
    let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    assert!(matches!(
        TextDetector::detect(jwt),
        ClipboardTextSyntax::JwtToken
    ));
    assert!(!matches!(
        TextDetector::detect("not.a.jwt"),
        ClipboardTextSyntax::JwtToken
    ));
}

#[test]
fn test_detect_color() {
    assert!(matches!(
        TextDetector::detect("#FF5733"),
        ClipboardTextSyntax::Color
    ));
    assert!(matches!(
        TextDetector::detect("#abc"),
        ClipboardTextSyntax::Color
    ));
    assert!(matches!(
        TextDetector::detect("rgb(255, 0, 0)"),
        ClipboardTextSyntax::Color
    ));
    assert!(matches!(
        TextDetector::detect("rgba(255, 0, 0, 0.5)"),
        ClipboardTextSyntax::Color
    ));
    assert!(matches!(
        TextDetector::detect("hsl(120, 100%, 50%)"),
        ClipboardTextSyntax::Color
    ));
    assert!(!matches!(
        TextDetector::detect("#ZZZ"),
        ClipboardTextSyntax::Color
    ));
}

#[test]
fn test_detect_ip_address() {
    assert!(matches!(
        TextDetector::detect("192.168.1.1"),
        ClipboardTextSyntax::IpAddress
    ));
    assert!(matches!(
        TextDetector::detect("127.0.0.1"),
        ClipboardTextSyntax::IpAddress
    ));
    assert!(matches!(
        TextDetector::detect("2001:0db8:85a3:0000:0000:8a2e:0370:7334"),
        ClipboardTextSyntax::IpAddress
    ));
    assert!(!matches!(
        TextDetector::detect("256.256.256.256"),
        ClipboardTextSyntax::IpAddress
    ));
    assert!(!matches!(
        TextDetector::detect("1.2.3"),
        ClipboardTextSyntax::IpAddress
    ));
}

#[test]
fn test_detect_file_path() {
    assert!(matches!(
        TextDetector::detect("C:\\Windows\\System32"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(matches!(
        TextDetector::detect("C:/Users/Name"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(matches!(
        TextDetector::detect("/usr/local/bin"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(matches!(
        TextDetector::detect("/home/user/.config"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(matches!(
        TextDetector::detect("\\\\Server\\Share"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(matches!(
        TextDetector::detect("~/Documents"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(!matches!(
        TextDetector::detect("http://example.com"),
        ClipboardTextSyntax::FilePath
    ));
    assert!(!matches!(
        TextDetector::detect("Just some text"),
        ClipboardTextSyntax::FilePath
    ));
}

#[test]
fn test_detect_mxgraph() {
    // Complete mxGraph node.
    let mxgraph_node = r#"<mxCell id="2" value="开始" style="ellipse;whiteSpace=wrap;html=1;fillColor=#d5e8d4;strokeColor=#82b366;" vertex="1" parent="1">
  <mxGeometry x="340" y="40" width="120" height="60" as="geometry"/>
</mxCell>"#;
    assert!(matches!(
        TextDetector::detect(mxgraph_node),
        ClipboardTextSyntax::MxGraph
    ));

    // mxGraph edge.
    let mxgraph_edge = r#"<mxCell id="edge1" edge="1" parent="1" source="2" target="3">
  <mxGeometry relative="1" as="geometry"/>
</mxCell>"#;
    assert!(matches!(
        TextDetector::detect(mxgraph_edge),
        ClipboardTextSyntax::MxGraph
    ));

    // Complete flowchart with multiple mxCell elements.
    let mxgraph_full = r#"<mxCell id="2" value="开始" style="ellipse;whiteSpace=wrap;html=1;fillColor=#d5e8d4;strokeColor=#82b366;" vertex="1" parent="1">
  <mxGeometry x="340" y="40" width="120" height="60" as="geometry"/>
</mxCell>
<mxCell id="3" value="用户访问登录页面" style="rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;" vertex="1" parent="1">
  <mxGeometry x="320" y="140" width="160" height="60" as="geometry"/>
</mxCell>
<mxCell id="edge1" edge="1" parent="1" source="2" target="3">
  <mxGeometry relative="1" as="geometry"/>
</mxCell>"#;
    assert!(matches!(
        TextDetector::detect(mxgraph_full),
        ClipboardTextSyntax::MxGraph
    ));

    // Inputs that must not be classified as mxGraph.

    // Ordinary XML without mxCell.
    let plain_xml = r#"<?xml version="1.0"?>
<root>
  <item id="1">test</item>
</root>"#;
    assert!(!matches!(
        TextDetector::detect(plain_xml),
        ClipboardTextSyntax::MxGraph
    ));

    // SVG is XML-based but is not mxGraph.
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
  <circle cx="50" cy="50" r="40" fill="red"/>
</svg>"#;
    assert!(!matches!(
        TextDetector::detect(svg),
        ClipboardTextSyntax::MxGraph
    ));

    // Text that contains `mxCell` without a valid tag.
    let fake_mxgraph = "This text mentions mxCell but is not mxGraph format";
    assert!(!matches!(
        TextDetector::detect(fake_mxgraph),
        ClipboardTextSyntax::MxGraph
    ));

    // An mxCell tag without a `vertex` or `edge` attribute.
    let invalid_mxcell = r#"<mxCell id="1">
  <mxGeometry x="0" y="0"/>
</mxCell>"#;
    assert!(!matches!(
        TextDetector::detect(invalid_mxcell),
        ClipboardTextSyntax::MxGraph
    ));
}
