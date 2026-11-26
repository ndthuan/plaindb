use plaindb_core::testgen::{build_store, document, id_for};
use plaindb_core::{Format, codec_for, version_of};
use tempfile::TempDir;

fn digest(format: Format, seed: u64) -> String {
    let mut bytes = Vec::new();
    for index in 0..200 {
        bytes.extend(
            codec_for(format)
                .encode(&document(format, seed, index))
                .unwrap(),
        );
    }
    version_of(&bytes)
}

#[test]
fn the_same_seed_gives_the_same_bytes() {
    for format in [Format::Json, Format::Yaml, Format::Md] {
        assert_eq!(digest(format, 7), digest(format, 7));
        assert_ne!(digest(format, 7), digest(format, 8));
    }
}

#[test]
fn the_generated_bytes_are_pinned() {
    assert_eq!(
        digest(Format::Json, 7),
        "bc4da1e7e3d9c8afb7b963423350a538d1bdb9a5"
    );
    assert_eq!(
        digest(Format::Yaml, 7),
        "2d3d824e851efa9dca09db6dde752638e65e0c13"
    );
    assert_eq!(
        digest(Format::Md, 7),
        "05f1d7a48c9491df9a4944dddecdd54384af3990"
    );
}

#[test]
fn documents_are_one_to_four_kilobytes_with_ten_fields() {
    for format in [Format::Json, Format::Yaml, Format::Md] {
        for index in 0..100 {
            let doc = document(format, 7, index);
            assert_eq!(doc.len(), 10);
            let size = codec_for(format).encode(&doc).unwrap().len();
            assert!((1000..=4400).contains(&size), "{size} bytes");
        }
    }
}

#[test]
fn the_store_holds_every_generated_document() {
    let dir = TempDir::new().unwrap();
    let store = build_store(dir.path(), Format::Md, 120, 7, true).unwrap();
    assert_eq!(store.list_ids("bench", "items").unwrap().len(), 120);
    assert!(store.get("bench", "items", &id_for(119)).unwrap().is_some());
}
