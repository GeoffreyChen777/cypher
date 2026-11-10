//! The service's pure pieces; the tools, views and passes run against a
//! real engine in `tests/memory.rs`.

use loro::LoroDoc;

use cypher_proto::{MEMORY_INDEX, MemoryAction, MemoryAuthor};

use super::doc::StoreDoc;
use super::{Planned, apply, check_limits, line_summary, user_key};

fn planned(path: &str, text: Option<&str>) -> Planned {
    Planned {
        path: path.into(),
        text: text.map(String::from),
        from: None,
    }
}

#[test]
fn line_summaries_say_what_changed() {
    assert_eq!(line_summary("a\nb\n", "a\nb\nc\n"), "Added 1 line");
    assert_eq!(line_summary("a\nb\nc\n", "a\n"), "Removed 2 lines");
    assert_eq!(line_summary("a\nb\n", "a\nB\n"), "Changed 1 line");
    assert_eq!(line_summary("a\n", "b\nc\n"), "Added 2 lines, removed 1");
    assert_eq!(line_summary("a\n", "a\n"), "Updated");
}

#[test]
fn the_user_key_is_stable_and_private() {
    let key = user_key("org", "user");
    assert_eq!(key, user_key("org", "user"));
    assert_ne!(key, user_key("org", "someone"));
    assert_eq!(key.len(), 16);
    assert!(!key.contains("user"));
}

#[test]
fn applying_logs_who_changed_what_and_flags_the_owners_text() {
    let doc = LoroDoc::new();
    let store = StoreDoc::new(&doc);
    apply(
        &store,
        &planned(MEMORY_INDEX, Some("- mine\n")),
        &MemoryAuthor::Owner,
    )
    .unwrap();
    let agent = MemoryAuthor::Session {
        chat_id: "c1".into(),
        title: Some("Fix the build".into()),
    };
    apply(
        &store,
        &planned(MEMORY_INDEX, Some("- mine, corrected\n")),
        &agent,
    )
    .unwrap();
    apply(&store, &planned("/testing.md", Some("cargo test")), &agent).unwrap();
    let changes = store.changes();
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].path, "/testing.md");
    assert_eq!(changes[0].action, MemoryAction::Create);
    assert!(!changes[0].owner_text);
    assert_eq!(changes[1].action, MemoryAction::Edit);
    assert!(changes[1].owner_text, "an agent changed the owner's line");
    assert_eq!(changes[1].before.as_deref(), Some("- mine"));
    assert!(changes[1].undo.is_some());
    assert_eq!(store.file(MEMORY_INDEX).unwrap().author, agent);
    // Deleting a missing file is refused, not logged.
    assert!(apply(&store, &planned("/nope.md", None), &agent).is_err());
    assert_eq!(store.changes().len(), 3);
}

#[test]
fn limits_are_refused_with_the_reason() {
    let doc = LoroDoc::new();
    let store = StoreDoc::new(&doc);
    let big = "x".repeat(cypher_proto::MEMORY_FILE_MAX + 1);
    let err = check_limits(&store, &planned("/big.md", Some(&big)))
        .unwrap_err()
        .to_string();
    assert!(err.contains("at most 64 KB"), "{err}");
    assert!(check_limits(&store, &planned("/ok.md", Some("fine"))).is_ok());
    assert!(check_limits(&store, &planned("/gone.md", None)).is_ok());
}
