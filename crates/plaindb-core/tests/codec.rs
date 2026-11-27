use plaindb_core::{Codec, Doc, ErrorCode, Format, codec_for, version_of};
use proptest::prelude::*;
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn codec(format: Format) -> &'static dyn Codec {
    codec_for(format)
}

#[test]
fn version_matches_git_blob_hash() {
    assert_eq!(
        version_of(b"hello\n"),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
    assert_eq!(version_of(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
}

#[test]
fn json_unsorted_input_is_rewritten_sorted() {
    let bytes = include_bytes!("fixtures/codec/unsorted.json");
    let decoded = codec(Format::Json).decode(bytes).unwrap();
    let encoded = codec(Format::Json).encode(&decoded).unwrap();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert!(text.ends_with("}\n"));
    assert!(
        text.starts_with(
            "{\n  \"_id\": \"k\",\n  \"alpha\": {\n    \"x\": \"H\u{e0} N\u{1ed9}i\","
        )
    );
    assert_eq!(codec(Format::Json).decode(&encoded).unwrap(), decoded);
    assert_eq!(codec(Format::Json).encode(&decoded).unwrap(), encoded);
}

#[test]
fn yaml_with_comments_decodes() {
    let bytes = include_bytes!("fixtures/codec/commented.yaml");
    let decoded = codec(Format::Yaml).decode(bytes).unwrap();
    assert_eq!(decoded["zeta"], json!(1));
    assert_eq!(decoded["alpha"]["x"], json!("Tr\u{1b0}\u{1edd}ng Sa"));
    let encoded = codec(Format::Yaml).encode(&decoded).unwrap();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert!(!text.contains('#'));
    assert!(text.find("alpha").unwrap() < text.find("zeta").unwrap());
    assert_eq!(codec(Format::Yaml).decode(&encoded).unwrap(), decoded);
}

#[test]
fn markdown_body_survives_byte_for_byte() {
    let bytes = include_bytes!("fixtures/codec/crlf-body.md");
    let decoded = codec(Format::Md).decode(bytes).unwrap();
    assert_eq!(
        decoded["body"],
        json!("line with trailing spaces   \r\nsecond line, no final newline")
    );
    assert_eq!(decoded["title"], json!("H\u{e0} N\u{1ed9}i"));
    let encoded = codec(Format::Md).encode(&decoded).unwrap();
    assert!(encoded.ends_with(b"\r\nsecond line, no final newline"));
    assert_eq!(codec(Format::Md).decode(&encoded).unwrap(), decoded);
    assert_eq!(codec(Format::Md).encode(&decoded).unwrap(), encoded);
}

#[test]
fn markdown_without_frontmatter_is_only_a_body() {
    let decoded = codec(Format::Md).decode(b"just text\n").unwrap();
    assert_eq!(decoded, doc(json!({"body": "just text\n"})));
    let empty = codec(Format::Md).decode(b"").unwrap();
    assert_eq!(empty, doc(json!({"body": ""})));
}

#[test]
fn markdown_missing_body_encodes_an_empty_body() {
    let encoded = codec(Format::Md).encode(&doc(json!({"a": 1}))).unwrap();
    assert_eq!(encoded, b"---\na: 1\n---\n");
    let decoded = codec(Format::Md).decode(&encoded).unwrap();
    assert_eq!(decoded, doc(json!({"a": 1, "body": ""})));
}

#[test]
fn markdown_rejects_a_bad_body_and_frontmatter() {
    let error = codec(Format::Md)
        .encode(&doc(json!({"body": 3})))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    assert_eq!(error.details["reason"], json!("body_not_string"));
    let error = codec(Format::Md)
        .decode(b"---\nbody: x\n---\ntext")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    let error = codec(Format::Md).decode(b"---\na: 1\n").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    let error = codec(Format::Md).decode(b"---\n- a\n---\n").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
}

#[test]
fn invalid_input_is_an_invalid_document() {
    for (format, bytes) in [
        (Format::Json, &b"{\"a\":"[..]),
        (Format::Json, &b"[1, 2]"[..]),
        (Format::Json, &b""[..]),
        (Format::Yaml, &b"- a\n- b\n"[..]),
        (Format::Yaml, &b"plain text"[..]),
        (Format::Yaml, &b"a: [1\n"[..]),
    ] {
        let error = codec(format).decode(bytes).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidDocument, "{format:?}");
    }
}

#[test]
fn vietnamese_text_round_trips_unchanged() {
    let original = doc(
        json!({"city": "H\u{e0} N\u{1ed9}i, Tr\u{1b0}\u{1edd}ng Sa", "body": "N\u{1ed9}i dung"}),
    );
    for format in [Format::Json, Format::Yaml, Format::Md] {
        let encoded = codec(format).encode(&original).unwrap();
        assert_eq!(
            codec(format).decode(&encoded).unwrap(),
            original,
            "{format:?}"
        );
    }
}

#[test]
fn system_fields_are_ordinary_keys() {
    let original = doc(json!({
        "_id": "a",
        "_created_at": "2026-01-01T00:00:00.000Z",
        "_updated_at": "2026-01-02T00:00:00.000Z",
        "n": 1
    }));
    for format in [Format::Json, Format::Yaml, Format::Md] {
        let encoded = codec(format).encode(&original).unwrap();
        let decoded = codec(format).decode(&encoded).unwrap();
        for key in ["_id", "_created_at", "_updated_at", "n"] {
            assert_eq!(decoded[key], original[key], "{format:?} {key}");
        }
    }
}

fn arbitrary_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|number| json!(number)),
        any::<u64>().prop_map(|number| json!(number)),
        (-1.0e9f64..1.0e9f64).prop_map(|number| json!(number)),
        any::<String>().prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map(any::<String>(), inner, 0..5)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

fn arbitrary_doc() -> impl Strategy<Value = Doc> {
    prop::collection::btree_map(any::<String>(), arbitrary_value(), 0..6)
        .prop_map(|map| map.into_iter().filter(|(key, _)| key != "body").collect())
}

proptest! {
    #[test]
    fn every_format_round_trips(original in arbitrary_doc(), body in any::<String>()) {
        for format in [Format::Json, Format::Yaml] {
            let encoded = codec(format).encode(&original).unwrap();
            let decoded = codec(format).decode(&encoded).unwrap();
            prop_assert_eq!(&decoded, &original);
            prop_assert_eq!(codec(format).encode(&decoded).unwrap(), encoded);
        }
        let mut with_body = original.clone();
        with_body.insert("body".to_owned(), Value::String(body));
        let encoded = codec(Format::Md).encode(&with_body).unwrap();
        let decoded = codec(Format::Md).decode(&encoded).unwrap();
        prop_assert_eq!(&decoded, &with_body);
        prop_assert_eq!(codec(Format::Md).encode(&decoded).unwrap(), encoded);
    }
}

#[test]
fn yaml_reads_only_core_booleans() {
    let decoded = codec(Format::Yaml)
        .decode(b"country: NO\nok: yes\nsw: Off\nt: true\nf: False\ny: Y\nn: n\non: ON\n")
        .unwrap();
    assert_eq!(
        Value::Object(decoded),
        json!({
            "country": "NO",
            "ok": "yes",
            "sw": "Off",
            "t": true,
            "f": false,
            "y": "Y",
            "n": "n",
            "on": "ON",
        })
    );
}

#[test]
fn yaml_keeps_leading_zeros_as_text() {
    let decoded = codec(Format::Yaml)
        .decode(b"zip: 010\nn: 10\nz: 0\nh: 0.5\ne: 1e3\nneg: -3\n")
        .unwrap();
    assert_eq!(
        Value::Object(decoded),
        json!({"zip": "010", "n": 10, "z": 0, "h": 0.5, "e": 1000.0, "neg": -3})
    );
}

#[test]
fn markdown_front_matter_reads_the_same_way() {
    let decoded = codec(Format::Md)
        .decode(b"---\ncountry: NO\nzip: 010\n---\ntext\n")
        .unwrap();
    assert_eq!(
        Value::Object(decoded),
        json!({"country": "NO", "zip": "010", "body": "text\n"})
    );
}

#[test]
fn a_byte_order_mark_is_dropped_in_every_format() {
    for (format, text) in [
        (Format::Json, &b"{\"title\": \"X\"}\n"[..]),
        (Format::Yaml, &b"title: X\n"[..]),
        (Format::Md, &b"---\ntitle: X\n---\ntext\n"[..]),
    ] {
        let mut marked = b"\xef\xbb\xbf".to_vec();
        marked.extend_from_slice(text);
        let plain = codec(format).decode(text).unwrap();
        assert_eq!(codec(format).decode(&marked).unwrap(), plain, "{format:?}");
        assert!(
            !codec(format)
                .encode(&plain)
                .unwrap()
                .starts_with(b"\xef\xbb\xbf")
        );
    }
    let marked = codec(Format::Md)
        .decode(b"\xef\xbb\xbf---\ntitle: X\n---\ntext\n")
        .unwrap();
    assert_eq!(
        Value::Object(marked),
        json!({"title": "X", "body": "text\n"})
    );
}

#[test]
fn markdown_fences_may_end_in_spaces_and_tabs() {
    let decoded = codec(Format::Md)
        .decode(b"--- \ntitle: X\n---\t\ntext\n")
        .unwrap();
    assert_eq!(
        Value::Object(decoded),
        json!({"title": "X", "body": "text\n"})
    );
    let decoded = codec(Format::Md)
        .decode(b"---  \t\ntitle: X\n--- \r\ntext\n")
        .unwrap();
    assert_eq!(
        Value::Object(decoded),
        json!({"title": "X", "body": "text\n"})
    );
}

#[test]
fn markdown_with_a_blank_line_before_the_fence_is_all_body() {
    let text = "\n---\ntitle: X\n---\ntext\n";
    let decoded = codec(Format::Md).decode(text.as_bytes()).unwrap();
    assert_eq!(Value::Object(decoded), json!({"body": text}));
}
