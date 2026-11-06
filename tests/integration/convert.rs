use dayone::convert::{RtjDocument, markdown_to_rtjson, rtjson_to_markdown};

fn load_rtjson(name: &str) -> RtjDocument {
    let path = format!("tests/fixtures/rtjson/{}", name);
    let json =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path, e));
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("failed to parse {}: {}", path, e))
}

fn load_md(name: &str) -> String {
    let path = format!("tests/fixtures/markdown/{}", name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path, e))
}

macro_rules! rtjson_to_md_test {
    ($name:ident, $num:literal) => {
        #[test]
        fn $name() {
            let doc = load_rtjson(concat!($num, ".json"));
            let expected = load_md(concat!($num, ".md"));
            assert_eq!(
                rtjson_to_markdown(&doc),
                expected,
                "fixture {} Markdown mismatch",
                $num
            );
        }
    };
}

rtjson_to_md_test!(fixture_01_plain_text, "01");
rtjson_to_md_test!(fixture_02_multiple_paragraphs, "02");
rtjson_to_md_test!(fixture_03_header_one, "03");
rtjson_to_md_test!(fixture_04_header_two, "04");
rtjson_to_md_test!(fixture_05_header_three, "05");
rtjson_to_md_test!(fixture_06_inline_formatting, "06");
rtjson_to_md_test!(fixture_07_code, "07");
rtjson_to_md_test!(fixture_08_highlight, "08");
rtjson_to_md_test!(fixture_09_links, "09");
rtjson_to_md_test!(fixture_10_bulleted_list, "10");
rtjson_to_md_test!(fixture_11_nested_list, "11");
rtjson_to_md_test!(fixture_12_numbered_list, "12");
rtjson_to_md_test!(fixture_13_checkbox, "13");
rtjson_to_md_test!(fixture_14_blockquote, "14");
rtjson_to_md_test!(fixture_15_photo, "15");
rtjson_to_md_test!(fixture_16_video_audio, "16");
rtjson_to_md_test!(fixture_17_pdf, "17");
rtjson_to_md_test!(fixture_18_external_video, "18");
rtjson_to_md_test!(fixture_19_horizontal_rule, "19");
rtjson_to_md_test!(fixture_20_table, "20");
rtjson_to_md_test!(fixture_21_mixed, "21");

// ── Markdown → RTJson → Markdown round-trip tests ──────────────────────────────

// Markdown → RTJson → Markdown must equal original
macro_rules! md_roundtrip_test {
    ($name:ident, $num:literal) => {
        #[test]
        fn $name() {
            let md = load_md(concat!($num, ".md"));
            let result = markdown_to_rtjson(&md);
            let back = rtjson_to_markdown(&result.document);
            assert_eq!(back, md, "fixture {} Markdown round-trip failed", $num);
        }
    };
}

md_roundtrip_test!(roundtrip_md_01, "01");
md_roundtrip_test!(roundtrip_md_02, "02");
md_roundtrip_test!(roundtrip_md_03, "03");
md_roundtrip_test!(roundtrip_md_04, "04");
md_roundtrip_test!(roundtrip_md_05, "05");
md_roundtrip_test!(roundtrip_md_06, "06");
md_roundtrip_test!(roundtrip_md_07, "07");
md_roundtrip_test!(roundtrip_md_08, "08");
md_roundtrip_test!(roundtrip_md_09, "09");
md_roundtrip_test!(roundtrip_md_10, "10");
md_roundtrip_test!(roundtrip_md_11, "11");
md_roundtrip_test!(roundtrip_md_13, "13");
md_roundtrip_test!(roundtrip_md_14, "14");
md_roundtrip_test!(roundtrip_md_20, "20");
md_roundtrip_test!(roundtrip_md_21, "21");

#[test]
fn roundtrip_md_12_canonicalizes_ordered_list_numbering_then_stabilizes() {
    let md = load_md("12.md");
    let once = rtjson_to_markdown(&markdown_to_rtjson(&md).document);
    let twice = rtjson_to_markdown(&markdown_to_rtjson(&once).document);
    assert_eq!(
        once, twice,
        "fixture 12 canonical markdown should stabilize"
    );
}

#[test]
fn has_tables_set_for_fixture_20() {
    let md = load_md("20.md");
    let result = markdown_to_rtjson(&md);
    assert!(
        result.has_tables,
        "fixture 20 (table) should set has_tables"
    );
}

#[test]
fn has_tables_false_for_fixture_01() {
    let md = load_md("01.md");
    let result = markdown_to_rtjson(&md);
    assert!(!result.has_tables);
}
