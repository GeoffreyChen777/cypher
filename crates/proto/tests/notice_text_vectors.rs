//! `compact_notice` / `strip_reply_notices` against
//! `protocol/vectors/notice-text-v1.json`, which the iPhone's twin
//! (`apps/ios/Cypher/Session/NoticeText.swift`, `NoticeTextTests`) runs too.

use cypher_proto::{compact_notice, is_reply_notice, strip_reply_notices};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../protocol/vectors/notice-text-v1.json"
    ))
    .unwrap()
}

#[test]
fn compact_vectors() {
    let vectors = vectors();
    let cases = vectors["compact"].as_array().unwrap();
    assert!(cases.len() >= 8);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        assert_eq!(
            compact_notice(text),
            case["expect"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn strip_vectors() {
    let vectors = vectors();
    let cases = vectors["strip"].as_array().unwrap();
    assert!(cases.len() >= 8);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        let expect = case["expect"].as_str();
        assert_eq!(strip_reply_notices(text).as_deref(), expect, "{name}");
        // A part that strips to nothing is exactly a whole-part notice.
        assert_eq!(is_reply_notice(text), expect == Some(""), "{name}");
    }
}
