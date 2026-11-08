use super::*;

#[test]
fn paths_are_relative_and_cannot_enter_git_metadata() {
    for path in [
        "/etc/passwd",
        "../secret",
        "a/../x",
        "a/./x",
        "a//x",
        ".git/config",
        ".GIT/config",
        "src/.Git/config",
        "x/.git/y",
        "x\0y",
    ] {
        assert!(components(path).is_err(), "{path:?}");
    }
    assert!(components("").unwrap().is_empty());
    assert_eq!(components("src/main.rs").unwrap(), ["src", "main.rs"]);
}

#[cfg(unix)]
#[test]
fn bounded_text_and_listing_reject_links_and_special_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("src/main.rs"), "hello\n世界").unwrap();
    std::fs::write(root.join("binary"), b"x\0y").unwrap();
    std::fs::write(root.join("latin"), [0xff]).unwrap();
    std::fs::write(root.join("large"), vec![b'a'; TEXT_LIMIT + 10]).unwrap();
    std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
    std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("link")).unwrap();
    assert!(text(&root, "escape/passwd").is_err());
    assert!(text(&root, "link").is_err());
    assert!(list(&root, "escape").is_err());
    assert!(text(&root, "src").is_err());
    assert_eq!(
        text(&root, "src/main.rs").unwrap().text.as_deref(),
        Some("hello\n世界")
    );
    assert!(text(&root, "binary").unwrap().binary);
    assert!(text(&root, "latin").unwrap().binary);
    let large = text(&root, "large").unwrap();
    assert!(large.truncated);
    assert_eq!(large.text.unwrap().len(), TEXT_LIMIT);
    let names = list(&root, "").unwrap();
    assert_eq!(names.entries[0].name, "src");
    assert!(
        !names
            .entries
            .iter()
            .any(|e| [".git", "escape", "link"].contains(&e.name.as_str()))
    );
    let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
    // SAFETY: fifo is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(text(&root, "fifo").is_err());
}

#[cfg(unix)]
#[test]
fn listings_carry_sizes_and_times_in_case_insensitive_order() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("b.txt"), "12345").unwrap();
    std::fs::write(root.join("A.txt"), "").unwrap();
    std::fs::write(root.join("c.txt"), "").unwrap();
    std::fs::create_dir(root.join("zeta")).unwrap();
    let listing = list(&root, "").unwrap();
    let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["zeta", "A.txt", "b.txt", "c.txt"]);
    assert_eq!(listing.entries[2].size, Some(5));
    assert_eq!(listing.entries[0].size, None);
    assert!(listing.entries.iter().all(|e| e.modified_ms.unwrap() > 0));
}

#[cfg(unix)]
#[test]
fn replace_rewrites_only_existing_regular_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "a much longer original body\n").unwrap();
    std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("link")).unwrap();
    std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();

    // Shorter text must not leave the old tail behind.
    assert_eq!(
        replace(&root, "src/main.rs", "fn main() {}").unwrap().bytes,
        12
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert_eq!(
        text(&root, "src/main.rs").unwrap().text.as_deref(),
        Some("fn main() {}")
    );

    // No creation, no links, no directories, no escaping the root.
    assert!(replace(&root, "src/new.rs", "x").is_err());
    assert!(!root.join("src/new.rs").exists());
    assert!(replace(&root, "link", "x").is_err());
    assert!(replace(&root, "escape/passwd", "x").is_err());
    assert!(replace(&root, "src", "x").is_err());
    assert!(replace(&root, "", "x").is_err());
    assert!(replace(&root, "../outside", "x").is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
    let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
    // SAFETY: fifo is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(replace(&root, "fifo", "x").is_err());
}

#[test]
fn truncated_utf8_does_not_split_a_character() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut data = vec![b'a'; TEXT_LIMIT - 1];
    data.extend_from_slice("世界".as_bytes());
    std::fs::write(root.join("utf8"), data).unwrap();
    let result = text(&root, "utf8").unwrap();
    assert!(result.truncated && !result.binary);
    assert_eq!(result.text.unwrap().len(), TEXT_LIMIT - 1);
}

#[test]
fn large_directories_are_explicitly_partial() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for i in 0..ENTRY_LIMIT + 1 {
        std::fs::write(root.join(format!("file-{i}")), "").unwrap();
    }
    let listing = list(&root, "").unwrap();
    assert!(listing.truncated);
    assert_eq!(listing.entries.len(), ENTRY_LIMIT);
}

#[test]
fn git_metadata_is_not_browsable_even_if_selected_as_a_workspace_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join(".git");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("config"), "metadata").unwrap();
    assert!(list(&root, "").is_err());
    assert!(text(&root, "config").is_err());
}

#[cfg(unix)]
#[test]
fn replacing_an_authorized_directory_with_a_link_does_not_escape() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let root = parent.join("checkout");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), "inside").unwrap();
    std::fs::rename(&root, parent.join("old")).unwrap();
    std::os::unix::fs::symlink("/etc", &root).unwrap();
    assert!(text(&root, "passwd").is_err());
    assert!(list(&root, "").is_err());
    assert!(create(&root, "new", false).is_err());
    assert!(delete(&root, "passwd", true).is_err());
}

#[cfg(unix)]
#[test]
fn chunks_read_every_byte_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let data: Vec<u8> = (0..CHUNK_BYTES * 2 + 17).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("blob.bin"), &data).unwrap();
    let mut read = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = read_chunk(&root, "blob.bin", offset).unwrap();
        assert_eq!(chunk.offset, offset);
        assert_eq!(chunk.bytes, data.len() as u64);
        read.extend(
            base64::engine::general_purpose::STANDARD
                .decode(&chunk.data)
                .unwrap(),
        );
        offset = chunk.next_offset;
        if chunk.done {
            break;
        }
    }
    assert_eq!(read, data);
    let empty = tempfile::NamedTempFile::new_in(&root).unwrap();
    let name = empty.path().file_name().unwrap().to_str().unwrap();
    let chunk = read_chunk(&root, name, 0).unwrap();
    assert!(chunk.done && chunk.data.is_empty());
    assert!(read_chunk(&root, "blob.bin", data.len() as u64 + 1).is_err());
    std::os::unix::fs::symlink(root.join("blob.bin"), root.join("link")).unwrap();
    assert!(read_chunk(&root, "link", 0).is_err());
}

#[cfg(unix)]
#[test]
fn create_makes_new_entries_and_never_replaces() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("existing"), "keep").unwrap();
    std::os::unix::fs::symlink("/tmp", root.join("escape")).unwrap();

    // Modes are the usual ones even under the engine's private umask.
    // SAFETY: umask only swaps the process mask (restored below).
    let previous = unsafe { libc::umask(0o077) };
    create(&root, "docs", true).unwrap();
    create(&root, "docs/readme.md", false).unwrap();
    write_chunk(&root, "docs/data.bin", 0, b"x").unwrap();
    // SAFETY: as above.
    unsafe { libc::umask(previous) };
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &str| {
        std::fs::metadata(root.join(p))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("docs"), 0o755);
    assert_eq!(mode("docs/readme.md"), 0o644);
    assert_eq!(mode("docs/data.bin"), 0o644);
    assert!(root.join("docs").is_dir());
    assert_eq!(std::fs::read(root.join("docs/readme.md")).unwrap(), b"");

    let conflict = create(&root, "existing", false).unwrap_err();
    assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(root.join("existing")).unwrap(), b"keep");
    assert!(create(&root, "docs", true).is_err());
    assert!(create(&root, "escape/new", false).is_err());
    assert!(create(&root, ".git", true).is_err());
    assert!(create(&root, "", true).is_err());
    assert!(create(&root, "missing/child", false).is_err());
    assert!(create(&root, &"n".repeat(NAME_LIMIT + 1), false).is_err());
}

#[cfg(unix)]
#[test]
fn chunked_writes_create_then_append_exactly_at_the_end() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("taken"), "keep").unwrap();
    assert_eq!(write_chunk(&root, "out.bin", 0, b"abc").unwrap().bytes, 3);
    assert_eq!(write_chunk(&root, "out.bin", 3, b"def").unwrap().bytes, 6);
    assert_eq!(std::fs::read(root.join("out.bin")).unwrap(), b"abcdef");
    // A replayed or skipped slice is refused rather than overwriting.
    assert!(write_chunk(&root, "out.bin", 3, b"xyz").is_err());
    assert!(write_chunk(&root, "out.bin", 9, b"xyz").is_err());
    // Offset 0 never replaces an existing file.
    assert!(write_chunk(&root, "taken", 0, b"new").is_err());
    assert_eq!(std::fs::read(root.join("taken")).unwrap(), b"keep");
    assert!(write_chunk(&root, "big", 0, &vec![0; WRITE_CHUNK_LIMIT + 1]).is_err());
}

#[cfg(unix)]
#[test]
fn moves_rename_without_replacing_and_stay_inside_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(root.join("b.txt"), "b").unwrap();
    std::os::unix::fs::symlink("/tmp", root.join("escape")).unwrap();

    move_entry(&root, "a.txt", "src/a.txt").unwrap();
    assert_eq!(std::fs::read(root.join("src/a.txt")).unwrap(), b"a");
    let conflict = move_entry(&root, "b.txt", "src/a.txt").unwrap_err();
    assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(root.join("src/a.txt")).unwrap(), b"a");
    assert!(move_entry(&root, "src", "src/nested/src").is_err());
    assert!(move_entry(&root, "b.txt", "escape/b.txt").is_err());
    assert!(move_entry(&root, "escape", "moved").is_err());
    assert!(move_entry(&root, "b.txt", "../b.txt").is_err());
    assert!(root.join("b.txt").exists());
    move_entry(&root, "src", "lib").unwrap();
    assert!(root.join("lib/nested").is_dir());
    // Case-only renames work on case-insensitive volumes too.
    move_entry(&root, "b.txt", "B.txt").unwrap();
    let names: Vec<_> = list(&root, "")
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"B.txt".to_string()), "{names:?}");
    assert!(!names.contains(&"b.txt".to_string()), "{names:?}");
}

#[cfg(unix)]
#[test]
fn copies_duplicate_trees_without_following_links() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("pkg/inner")).unwrap();
    std::fs::create_dir(root.join("pkg/.git")).unwrap();
    std::fs::write(root.join("pkg/.git/config"), "meta").unwrap();
    std::fs::write(root.join("pkg/inner/data.txt"), "data").unwrap();
    std::os::unix::fs::symlink("/etc", root.join("pkg/etc-link")).unwrap();
    std::fs::write(root.join("one.txt"), "one").unwrap();

    copy_entry(&root, "one.txt", "two.txt").unwrap();
    assert_eq!(std::fs::read(root.join("two.txt")).unwrap(), b"one");
    assert_eq!(
        copy_entry(&root, "one.txt", "two.txt").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        root.join("pkg/inner"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    copy_entry(&root, "pkg", "pkg copy").unwrap();
    assert_eq!(
        std::fs::metadata(root.join("pkg copy/inner"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::read(root.join("pkg copy/inner/data.txt")).unwrap(),
        b"data"
    );
    // The link is copied as a link, and git metadata is left behind.
    let link = std::fs::symlink_metadata(root.join("pkg copy/etc-link")).unwrap();
    assert!(link.file_type().is_symlink());
    assert!(!root.join("pkg copy/.git").exists());
    assert!(copy_entry(&root, "pkg", "pkg/inner/again").is_err());
    assert!(copy_entry(&root, "pkg/etc-link", "etc").is_err());
}

#[cfg(unix)]
#[test]
fn permanent_deletes_remove_trees_but_not_link_targets() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), "keep").unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("build/deep/deeper")).unwrap();
    std::fs::write(root.join("build/deep/deeper/x.o"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("build/outside-link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("top-link")).unwrap();
    std::fs::write(root.join("file.txt"), "f").unwrap();

    delete(&root, "file.txt", true).unwrap();
    assert!(!root.join("file.txt").exists());
    let gone = delete(&root, "build", true).unwrap();
    assert!(!gone.trashed);
    assert!(!root.join("build").exists());
    assert_eq!(
        std::fs::read(outside.path().join("precious")).unwrap(),
        b"keep"
    );
    // Links, the root, and git metadata are not deletable entries.
    assert!(delete(&root, "top-link", true).is_err());
    assert!(delete(&root, "", true).is_err());
    assert!(delete(&root, ".git", true).is_err());
    assert!(delete(&root, "missing", true).is_err());
}

#[test]
fn trash_names_follow_finder() {
    assert_eq!(trash_name("notes.md", "14.03.27", 0), "notes 14.03.27.md");
    assert_eq!(trash_name("notes.md", "14.03.27", 1), "notes 14.03.27 2.md");
    assert_eq!(trash_name(".env", "14.03.27", 0), ".env 14.03.27");
    assert_eq!(trash_name("Makefile", "14.03.27", 0), "Makefile 14.03.27");
    assert_eq!(percent_encode_path("/a b/ü.txt"), "/a%20b/%C3%BC.txt");
    assert!(within("src", "src/a"));
    assert!(within("src", "src"));
    assert!(!within("src", "src2/a"));
}
