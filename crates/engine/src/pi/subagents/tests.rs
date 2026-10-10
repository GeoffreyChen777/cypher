use super::*;

fn paths(root: &Path) -> PiRuntimePaths {
    let current = root.join("current");
    PiRuntimePaths {
        root: root.into(),
        current: current.clone(),
        executable: current.join("bin/pi"),
        npm_executable: current.join("bin/npm"),
        package_dir: current.join("pi"),
        agent_dir: root.join("agent"),
    }
}

fn sample() -> PiSubagent {
    PiSubagent {
        name: "planner".into(),
        description: "Resolve a design decision".into(),
        system_prompt: "You are the design-decision specialist.".into(),
        tools: vec!["read".into(), "bash".into()],
        model: Some("claude-bridge/claude-fable-5-1".into()),
        thinking: Some("xhigh".into()),
        read_only: true,
        builtin: false,
        overrides_builtin: false,
    }
}

/// The real `planner.md` closes its frontmatter with the body abutting the
/// delimiter (`---You are …`), which the extension's regex accepts.
#[test]
fn parses_a_body_abutting_the_closing_delimiter() {
    let text = "---\nname: planner\ndescription: Plan\n---You are the planner.\n";
    let parsed = parse(text).expect("parses");
    assert_eq!(parsed.agent.name, "planner");
    assert_eq!(parsed.agent.system_prompt, "You are the planner.");
}

#[test]
fn parses_the_conventional_form() {
    let text = "---\nname: planner\ndescription: Plan\n---\n\nYou are the planner.\n";
    let parsed = parse(text).expect("parses");
    assert_eq!(parsed.agent.system_prompt, "You are the planner.");
}

#[test]
fn skips_files_the_extension_would_skip() {
    // No frontmatter at all.
    assert!(parse("You are the planner.\n").is_none());
    // Missing description.
    assert!(parse("---\nname: planner\n---\nBody\n").is_none());
    // Missing name.
    assert!(parse("---\ndescription: Plan\n---\nBody\n").is_none());
    // Unsafe name.
    assert!(parse("---\nname: main\ndescription: Plan\n---\nBody\n").is_none());
    assert!(parse("---\nname: a b\ndescription: Plan\n---\nBody\n").is_none());
}

#[test]
fn reads_tools_as_one_comma_separated_scalar() {
    let parsed =
        parse("---\nname: a\ndescription: d\ntools: read, bash , grep\n---\nB\n").expect("parses");
    assert_eq!(parsed.agent.tools, ["read", "bash", "grep"]);
}

#[test]
fn honors_both_spellings_of_read_only() {
    let readonly = parse("---\nname: a\ndescription: d\nreadonly: TRUE\n---\nB\n").expect("parses");
    assert!(readonly.agent.read_only);
    let access =
        parse("---\nname: a\ndescription: d\naccess: Read-Only\n---\nB\n").expect("parses");
    assert!(access.agent.read_only);
    let neither = parse("---\nname: a\ndescription: d\n---\nB\n").expect("parses");
    assert!(!neither.agent.read_only);
}

#[test]
fn strips_one_layer_of_quotes_like_the_extension() {
    let parsed = parse("---\nname: a\ndescription: \"A quoted one\"\n---\nB\n").expect("parses");
    assert_eq!(parsed.agent.description, "A quoted one");
}

#[test]
fn a_description_may_contain_colons() {
    let parsed = parse("---\nname: a\ndescription: Do this: then that\n---\nB\n").expect("parses");
    assert_eq!(parsed.agent.description, "Do this: then that");
}

#[test]
fn round_trips_through_render() {
    let agent = sample();
    let parsed = parse(&render(&agent, &[])).expect("parses");
    assert_eq!(parsed.agent, agent);
}

#[test]
fn clearing_read_only_erases_a_stale_access_key() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    std::fs::create_dir_all(user_dir(&paths)).unwrap();
    std::fs::write(
        user_dir(&paths).join("a.md"),
        "---\nname: a\ndescription: d\naccess: read-only\n---\nBody\n",
    )
    .unwrap();
    assert!(list(&paths)[0].read_only);

    let mut agent = list(&paths).into_iter().next().unwrap();
    agent.read_only = false;
    save(&paths, &agent, Some("a")).unwrap();

    let text = std::fs::read_to_string(user_dir(&paths).join("a.md")).unwrap();
    assert!(
        !text.contains("access:"),
        "stale access key survived: {text}"
    );
    assert!(!list(&paths)[0].read_only);
}

#[test]
fn preserves_unknown_frontmatter_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    std::fs::create_dir_all(user_dir(&paths)).unwrap();
    std::fs::write(
        user_dir(&paths).join("a.md"),
        "---\nname: a\ndescription: d\ncolor: blue\n---\nBody\n",
    )
    .unwrap();

    let mut agent = list(&paths).into_iter().next().unwrap();
    agent.description = "changed".into();
    save(&paths, &agent, Some("a")).unwrap();

    let text = std::fs::read_to_string(user_dir(&paths).join("a.md")).unwrap();
    assert!(text.contains("color: blue"), "{text}");
    assert!(text.contains("description: changed"), "{text}");
}

#[test]
fn a_user_profile_overrides_a_builtin_of_the_same_name() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    std::fs::create_dir_all(builtin_dir(&paths)).unwrap();
    std::fs::write(
        builtin_dir(&paths).join("planner.md"),
        "---\nname: planner\ndescription: built in\n---\nBuiltin body\n",
    )
    .unwrap();

    let listed = list(&paths);
    assert_eq!(listed.len(), 1);
    assert!(listed[0].builtin);
    assert_eq!(listed[0].description, "built in");

    let mut agent = listed.into_iter().next().unwrap();
    agent.description = "mine".into();
    save(&paths, &agent, Some("planner")).unwrap();

    let listed = list(&paths);
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].builtin);
    assert!(listed[0].overrides_builtin);
    assert_eq!(listed[0].description, "mine");

    // Deleting the override restores the built-in rather than the agent
    // disappearing.
    delete(&paths, "planner").unwrap();
    let listed = list(&paths);
    assert_eq!(listed.len(), 1);
    assert!(listed[0].builtin);
    assert_eq!(listed[0].description, "built in");
}

#[test]
fn a_builtin_cannot_be_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    std::fs::create_dir_all(builtin_dir(&paths)).unwrap();
    std::fs::write(
        builtin_dir(&paths).join("planner.md"),
        "---\nname: planner\ndescription: built in\n---\nBody\n",
    )
    .unwrap();
    assert!(delete(&paths, "planner").is_err());
    assert_eq!(list(&paths).len(), 1);
}

#[test]
fn renaming_moves_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let mut agent = sample();
    save(&paths, &agent, None).unwrap();
    assert!(user_dir(&paths).join("planner.md").is_file());

    agent.name = "strategist".into();
    save(&paths, &agent, Some("planner")).unwrap();
    assert!(!user_dir(&paths).join("planner.md").exists());
    assert!(user_dir(&paths).join("strategist.md").is_file());
    let listed = list(&paths);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "strategist");
}

#[test]
fn a_rename_cannot_clobber_another_profile() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    save(&paths, &sample(), None).unwrap();
    let mut other = sample();
    other.name = "reviewer".into();
    other.description = "Review".into();
    save(&paths, &other, None).unwrap();

    other.name = "planner".into();
    let error = save(&paths, &other, Some("reviewer")).expect_err("must refuse");
    assert!(error.contains("already exists"), "{error}");
    // Both survive, unchanged.
    assert_eq!(list(&paths).len(), 2);
    assert_eq!(list(&paths)[0].description, "Resolve a design decision");
}

#[test]
fn creating_over_an_existing_name_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    save(&paths, &sample(), None).unwrap();
    assert!(save(&paths, &sample(), None).is_err());
}

#[test]
fn identity_is_the_frontmatter_name_not_the_filename() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    std::fs::create_dir_all(user_dir(&paths)).unwrap();
    std::fs::write(
        user_dir(&paths).join("misnamed.md"),
        "---\nname: planner\ndescription: d\n---\nBody\n",
    )
    .unwrap();
    assert_eq!(list(&paths)[0].name, "planner");
    delete(&paths, "planner").unwrap();
    assert!(!user_dir(&paths).join("misnamed.md").exists());
}

#[test]
fn validation_rejects_what_the_extension_would_drop() {
    let mut agent = sample();
    agent.description = "  ".into();
    assert!(agent.validate().is_err());

    let mut agent = sample();
    agent.name = "has space".into();
    assert!(agent.validate().is_err());

    let mut agent = sample();
    agent.description = "two\nlines".into();
    assert!(agent.validate().is_err());

    let mut agent = sample();
    agent.tools = vec!["read,write".into()];
    assert!(agent.validate().is_err());

    assert!(sample().validate().is_ok());
}

#[test]
fn a_saved_profile_is_owner_only() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    save(&paths, &sample(), None).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(user_dir(&paths).join("planner.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "profile is readable by others");
    }
}

#[test]
fn a_missing_runtime_lists_nothing() {
    let temp = tempfile::tempdir().unwrap();
    assert!(list(&paths(temp.path())).is_empty());
}
