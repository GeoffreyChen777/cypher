use super::file_index::{
    nucleo_path_score, search_files_blocking, search_files_blocking_with_cancel,
    search_files_cached,
};
use super::*;

#[test]
fn expand_home_only_rewrites_a_leading_tilde() {
    let home = cypher_env::home_dir().to_string_lossy().into_owned();
    assert_eq!(expand_home("~"), home);
    assert_eq!(expand_home("~/code"), format!("{home}/code"));
    // Absolute paths, relative paths, and a `~user` form (which this host
    // cannot resolve) all pass through untouched.
    assert_eq!(expand_home("/tmp/repo"), "/tmp/repo");
    assert_eq!(expand_home("~other/code"), "~other/code");
    assert_eq!(expand_home(""), "");
}

#[test]
fn chat_worktree_name_is_stable_and_unique_per_chat() {
    // Same chat key → same name (retry/duplicate Run reuse, never a second
    // worktree)…
    assert_eq!(
        chat_worktree_name("chat-abc", None),
        chat_worktree_name("chat-abc", None)
    );
    // …different chat keys differ (two chats never share a worktree)…
    assert_ne!(
        chat_worktree_name("chat-abc", None),
        chat_worktree_name("chat-def", None)
    );
    // The name carries a slugged key and an 8-hex hash suffix, and the
    // optional name hint only affects the slug, never the hash (reuse key).
    let named = chat_worktree_name("chat-abc", Some("Fix the Panel"));
    assert_eq!(
        named,
        "fix-the-panel-".to_string()
            + chat_worktree_name("chat-abc", None)
                .rsplit('-')
                .next()
                .unwrap()
    );
    assert_eq!(named.len(), "fix-the-panel".len() + 1 + 8);
    // A name hint is cosmetic: same chat key + different hints still
    // differ only in the slug portion.
    assert_ne!(named, chat_worktree_name("chat-abc", None));
    // Non-ASCII/empty keys still produce a valid folder name.
    let weird = chat_worktree_name("", None);
    assert_eq!(weird, chat_worktree_name("", None));
    assert!(!weird.is_empty());
    assert!(
        chat_worktree_name("a/b c", None)
            .chars()
            .all(|c| { c.is_ascii_alphanumeric() || c == '-' })
    );
}

#[test]
fn fuzzy_score_matches_a_path_subsequence() {
    // nucleo's match_paths ranking keeps the old subsequence contract:
    // multi-word queries and path subsequences still match, garbage does not.
    assert!(nucleo_path_score("cmp rs", "crates/ui/src/composer.rs").is_some());
    assert!(nucleo_path_score("composer crates", "crates/ui/src/composer.rs").is_some());
    assert!(nucleo_path_score("xyzq", "crates/ui/src/composer.rs").is_none());
}

#[test]
fn media_library_detection_covers_bundles_and_nested_roots() {
    for gated in [
        "/Users/x/Music/Music",
        "/Users/x/Music/iTunes",
        "/Users/x/Movies/TV",
        "/Users/x/Pictures/Photos Library.photoslibrary",
        "/Users/x/Pictures/Old iPhoto.photolibrary",
        "/Users/x/Pictures/Photo Booth Library",
        "/Users/x/Movies/Holiday.imovielibrary",
        "/Users/x/Music/Mixes.musiclibrary",
    ] {
        assert!(is_media_library(Path::new(gated)), "{gated}");
    }
    for browsable in [
        // The media folders themselves are not gated.
        "/Users/x/Music",
        "/Users/x/Pictures",
        // Real checkouts that merely live near them keep their badge.
        "/Users/x/Music/Band Practice",
        "/Users/x/code/Music",
        "/Users/x/code/tvlibrary",
    ] {
        assert!(!is_media_library(Path::new(browsable)), "{browsable}");
    }
}

#[test]
fn the_folder_browser_never_probes_inside_a_media_library() {
    // A planted `.git` makes the skip observable: a probe would report it.
    let home = tempfile::tempdir().unwrap();
    let music = home.path().join("Music");
    std::fs::create_dir_all(music.join("Music").join(".git")).unwrap();
    std::fs::create_dir_all(music.join("Band Practice").join(".git")).unwrap();

    let listing = list_folders_blocking(&music).unwrap();
    let entry = |name: &str| {
        listing
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {listing:?}"))
    };
    // The library is still listed and still navigable...
    assert!(entry("Music").is_dir);
    // ...but was never stat'ed for `.git`, which is what raised the prompt.
    assert!(!entry("Music").is_repo);
    // A neighbouring checkout is unaffected.
    assert!(entry("Band Practice").is_repo);
}

#[test]
fn the_file_index_skips_media_libraries() {
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("Music").join("Music");
    std::fs::create_dir_all(library.join("Media.localized")).unwrap();
    std::fs::write(library.join("Media.localized").join("track.m4a"), "").unwrap();
    std::fs::write(root.path().join("Music").join("notes.md"), "").unwrap();
    let cache: FileIndexCache = std::sync::Mutex::new(HashMap::new());

    assert!(
        search_files_cached(&cache, root.path(), "track", &[], || false)
            .unwrap()
            .is_empty(),
        "the walk descended into the media library"
    );
    assert!(
        !search_files_cached(&cache, root.path(), "notes", &[], || false)
            .unwrap()
            .is_empty(),
        "ordinary files beside it still index"
    );
}

#[test]
fn cached_search_reuses_one_walk_within_the_ttl() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.rs"), "").unwrap();
    let cache: FileIndexCache = std::sync::Mutex::new(HashMap::new());

    // First query walks and indexes the checkout.
    let first = search_files_cached(&cache, root.path(), "alpha", &[], || false).unwrap();
    assert_eq!(first.first().map(|m| m.path.as_str()), Some("alpha.rs"));
    // A file created after the walk is invisible to the next query: it
    // ranks against the cached index instead of re-walking.
    std::fs::write(root.path().join("beta.rs"), "").unwrap();
    let second = search_files_cached(&cache, root.path(), "beta", &[], || false).unwrap();
    assert!(second.is_empty(), "{second:?}");
}

#[test]
fn search_files_obeys_gitignore_and_returns_directories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/composer.rs"), "").unwrap();
    std::fs::write(root.path().join(".secret"), "").unwrap();
    std::fs::write(root.path().join(".gitignore"), "ignored\n").unwrap();
    std::fs::create_dir(root.path().join("ignored")).unwrap();
    std::fs::write(root.path().join("ignored/nope.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "src", &[]).unwrap();
    assert!(
        matches
            .iter()
            .any(|entry| entry.path == "src" && entry.is_dir)
    );
    assert!(matches.iter().any(|entry| entry.path == "src/composer.rs"));
    assert!(
        !matches
            .iter()
            .any(|entry| entry.path.starts_with("ignored"))
    );
    assert!(
        search_files_blocking(root.path(), "secret", &[])
            .unwrap()
            .iter()
            .any(|entry| entry.path == ".secret")
    );
    assert!(!matches.iter().any(|entry| entry.path.starts_with(".git/")));
}

#[test]
fn empty_search_features_recent_paths_before_shallow_entries() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("README.md"), "").unwrap();
    std::fs::write(root.path().join("src/deep.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "", &["src/deep.rs".into()]).unwrap();
    assert_eq!(
        matches.first().map(|entry| entry.path.as_str()),
        Some("src/deep.rs")
    );
    assert!(matches.iter().any(|entry| entry.path == "README.md"));
}

#[test]
fn search_files_prefers_filename_matches() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("composer/docs")).unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("composer/docs/readme.md"), "").unwrap();
    std::fs::write(root.path().join("src/composer.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "composer", &[]).unwrap();
    let composer = matches
        .iter()
        .position(|entry| entry.path == "src/composer.rs")
        .unwrap();
    let path_only = matches
        .iter()
        .position(|entry| entry.path == "composer/docs/readme.md")
        .unwrap();
    assert!(composer < path_only);
}

#[test]
fn cancelled_search_stops_before_walking() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("README.md"), "").unwrap();
    let cancelled = AtomicBool::new(true);

    let err = search_files_blocking_with_cancel(root.path(), "", &[], || {
        cancelled.load(Ordering::Relaxed)
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("cancelled"));
}

#[tokio::test]
async fn workspace_checkout_rejects_sibling_paths() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sibling = tempfile::tempdir().unwrap();
    let repos = Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));

    assert_eq!(
        repos.workspace_checkout(root.path(), root.path()).await,
        std::fs::canonicalize(root.path()).ok()
    );
    assert!(
        repos
            .workspace_checkout(root.path(), sibling.path())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn concurrent_searches_of_one_checkout_do_not_cancel_each_other() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.rs"), "").unwrap();
    std::fs::write(root.path().join("beta.rs"), "").unwrap();
    let repos = Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));

    let alpha = repos.search_files(root.path().into(), "alpha".into(), Vec::new());
    let beta = repos.search_files(root.path().into(), "beta".into(), Vec::new());
    let (alpha, beta) = tokio::join!(alpha, beta);

    assert_eq!(alpha.unwrap()[0].path, "alpha.rs");
    assert_eq!(beta.unwrap()[0].path, "beta.rs");
}
