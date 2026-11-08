use super::*;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn rehome_rewrites_only_the_header_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("staged");
    let body = "{\"type\":\"message\",\"id\":\"e1\",\"text\":\"cwd /old stays\"}\n{\"type\":\"message\",\"id\":\"e2\"}\n";
    write(
        &staged,
        &format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"s1\",\"cwd\":\"/old\",\"parentSession\":\"/p.jsonl\"}}\n{body}"
        ),
    );
    let target = dir.path().join("s1.jsonl");
    rehome_pi_session(&staged, &target, "/new/project").unwrap();

    let text = std::fs::read_to_string(&target).unwrap();
    let (header, rest) = text.split_once('\n').unwrap();
    let header: serde_json::Value = serde_json::from_str(header).unwrap();
    assert_eq!(header["cwd"], "/new/project");
    assert_eq!(header["id"], "s1");
    assert_eq!(header["version"], 3);
    assert_eq!(header["parentSession"], "/p.jsonl");
    assert_eq!(rest, body);
    assert!(!target.with_extension("jsonl.part").exists());
}

#[test]
fn rehome_refuses_a_file_that_is_not_a_pi_session() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("out.jsonl");
    for text in ["{\"type\":\"message\"}\n", "not json\n", ""] {
        let staged = dir.path().join("staged");
        write(&staged, text);
        let err = rehome_pi_session(&staged, &target, "/new").unwrap_err();
        assert!(err.to_string().contains("not a Pi session"), "{err}");
        assert!(!target.exists());
    }
}

#[test]
fn session_target_keeps_the_name_unless_taken() {
    let dir = tempfile::tempdir().unwrap();
    let free = session_target(dir.path(), "2026-10-28T00-00-00_abc.jsonl");
    assert_eq!(free, dir.path().join("2026-10-28T00-00-00_abc.jsonl"));
    write(&free, "taken");
    let next = session_target(dir.path(), "2026-10-28T00-00-00_abc.jsonl");
    assert_ne!(next, free);
    assert!(next.starts_with(dir.path()));
    let name = next.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("2026-10-28T00-00-00_abc-") && name.ends_with(".jsonl"));
    // A hostile or odd name never escapes the root.
    for odd in ["../../etc/passwd", "a b.jsonl", "x.txt"] {
        let path = session_target(dir.path(), odd);
        assert_eq!(path.parent(), Some(dir.path()), "{odd}");
        assert!(path.to_string_lossy().ends_with(".jsonl"));
    }
}

#[test]
fn staging_keys_are_file_name_safe() {
    assert!(valid_key("s0").is_ok());
    assert!(valid_key("3f1c-aa_9").is_ok());
    for bad in ["", "../x", "a/b", "a.b", &"k".repeat(65)] {
        assert!(valid_key(bad).is_err(), "{bad}");
    }
}
