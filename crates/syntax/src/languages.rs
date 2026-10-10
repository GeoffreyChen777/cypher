//! The per-language table: tree-sitter grammars and queries for every
//! [`LanguageId`], the injections each language embeds, and detection from a
//! fence alias, a file path or a shebang line.

use std::path::Path;

use tree_sitter_highlight::HighlightConfiguration;

use crate::{HighlightError, LanguageId};

pub fn injected_languages(parent: LanguageId) -> Vec<LanguageId> {
    use LanguageId::*;
    match parent {
        Html => vec![JavaScript, Css, Json],
        Markdown => vec![
            Rust, JavaScript, Jsx, TypeScript, Tsx, Python, Go, Json, Jsonc, Bash, Toml, Html, Css,
            Yaml, C, Cpp, CSharp, Java, Kotlin, Swift, Ruby, Php, Sql, Lua, Dockerfile, Nix, Make,
        ],
        _ => Vec::new(),
    }
}

pub fn rust_configuration() -> Result<HighlightConfiguration, HighlightError> {
    // The upstream Rust query groups numbers and booleans as
    // `constant.builtin`. Cypher preserves those structural roles separately.
    let highlights = tree_sitter_rust::HIGHLIGHTS_QUERY
        .replace(
            "(boolean_literal) @constant.builtin",
            "(boolean_literal) @boolean",
        )
        .replace(
            "(integer_literal) @constant.builtin",
            "(integer_literal) @number",
        )
        .replace(
            "(float_literal) @constant.builtin",
            "(float_literal) @number",
        );
    HighlightConfiguration::new(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        &highlights,
        tree_sitter_rust::INJECTIONS_QUERY,
        "",
    )
    .map_err(|error| HighlightError::Parser(error.to_string()))
}

fn make_configuration(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
) -> Result<HighlightConfiguration, HighlightError> {
    HighlightConfiguration::new(language, name, highlights, injections, locals)
        .map_err(|error| HighlightError::Parser(error.to_string()))
}

pub fn configuration(language: LanguageId) -> Result<HighlightConfiguration, HighlightError> {
    use LanguageId::*;
    match language {
        Rust => rust_configuration(),
        JavaScript => make_configuration(
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        Jsx => make_configuration(
            tree_sitter_javascript::LANGUAGE.into(),
            "jsx",
            &format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        TypeScript => make_configuration(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        Tsx => make_configuration(
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "tsx",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        Python => make_configuration(
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Go => make_configuration(
            tree_sitter_go::LANGUAGE.into(),
            "go",
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Json | Jsonc => make_configuration(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Bash => make_configuration(
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        Toml => make_configuration(
            tree_sitter_toml_ng::LANGUAGE.into(),
            "toml",
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Markdown => make_configuration(
            tree_sitter_md::LANGUAGE.into(),
            "markdown",
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            "",
        ),
        Html => make_configuration(
            tree_sitter_html::LANGUAGE.into(),
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        ),
        Css => make_configuration(
            tree_sitter_css::LANGUAGE.into(),
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Yaml => make_configuration(
            tree_sitter_yaml::LANGUAGE.into(),
            "yaml",
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        C => make_configuration(
            tree_sitter_c::LANGUAGE.into(),
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        Cpp => make_configuration(
            tree_sitter_cpp::LANGUAGE.into(),
            "cpp",
            &format!(
                "{}\n{}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            ),
            "",
            "",
        ),
        CSharp => make_configuration(
            tree_sitter_c_sharp::LANGUAGE.into(),
            "csharp",
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Java => make_configuration(
            tree_sitter_java::LANGUAGE.into(),
            "java",
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Kotlin => make_configuration(
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            "kotlin",
            "[(line_comment) (block_comment)] @comment [(string_literal) (multiline_string_literal)] @string [(number_literal) (float_literal)] @number",
            "",
            "",
        ),
        Swift => make_configuration(
            tree_sitter_swift::LANGUAGE.into(),
            "swift",
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_swift::LOCALS_QUERY,
        ),
        Ruby => make_configuration(
            tree_sitter_ruby::LANGUAGE.into(),
            "ruby",
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY,
        ),
        Php => make_configuration(
            tree_sitter_php::LANGUAGE_PHP.into(),
            "php",
            tree_sitter_php::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Sql => make_configuration(
            tree_sitter_sequel::LANGUAGE.into(),
            "sql",
            tree_sitter_sequel::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Lua => make_configuration(
            tree_sitter_lua::LANGUAGE.into(),
            "lua",
            tree_sitter_lua::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_lua::LOCALS_QUERY,
        ),
        Nix => make_configuration(
            tree_sitter_nix::LANGUAGE.into(),
            "nix",
            tree_sitter_nix::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Make => make_configuration(
            tree_sitter_make::LANGUAGE.into(),
            "make",
            tree_sitter_make::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        Dockerfile => make_configuration(
            tree_sitter_containerfile::LANGUAGE.into(),
            "dockerfile",
            tree_sitter_containerfile::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
    }
}

pub fn detect_language(
    path: Option<&str>,
    fence_tag: Option<&str>,
    first_line: Option<&str>,
) -> Option<LanguageId> {
    fence_tag
        .and_then(language_for_alias)
        .or_else(|| path.and_then(language_for_path))
        .or_else(|| first_line.and_then(language_for_shebang))
}

pub fn language_for_alias(alias: &str) -> Option<LanguageId> {
    let alias = alias
        .trim()
        .split_ascii_whitespace()
        .next()?
        .to_ascii_lowercase();
    Some(match alias.as_str() {
        "rust" | "rs" => LanguageId::Rust,
        "javascript" | "js" | "mjs" | "cjs" => LanguageId::JavaScript,
        "jsx" => LanguageId::Jsx,
        "typescript" | "ts" | "mts" | "cts" => LanguageId::TypeScript,
        "tsx" => LanguageId::Tsx,
        "python" | "py" | "python3" => LanguageId::Python,
        "go" | "golang" => LanguageId::Go,
        "json" => LanguageId::Json,
        "jsonc" => LanguageId::Jsonc,
        "bash" | "sh" | "shell" | "zsh" | "console" => LanguageId::Bash,
        "toml" => LanguageId::Toml,
        "markdown" | "md" => LanguageId::Markdown,
        "html" | "htm" => LanguageId::Html,
        "css" => LanguageId::Css,
        "yaml" | "yml" => LanguageId::Yaml,
        "c" => LanguageId::C,
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => LanguageId::Cpp,
        "csharp" | "c#" | "cs" => LanguageId::CSharp,
        "java" => LanguageId::Java,
        "kotlin" | "kt" | "kts" => LanguageId::Kotlin,
        "swift" => LanguageId::Swift,
        "ruby" | "rb" => LanguageId::Ruby,
        "php" => LanguageId::Php,
        "sql" => LanguageId::Sql,
        "lua" => LanguageId::Lua,
        "dockerfile" | "docker" => LanguageId::Dockerfile,
        "nix" => LanguageId::Nix,
        "make" | "makefile" => LanguageId::Make,
        _ => return None,
    })
}

pub fn language_for_path(path: &str) -> Option<LanguageId> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?;
    match name.to_ascii_lowercase().as_str() {
        "dockerfile" | "containerfile" => return Some(LanguageId::Dockerfile),
        "makefile" | "gnumakefile" => return Some(LanguageId::Make),
        "cargo.lock" | "cargo.toml" | "pyproject.toml" => return Some(LanguageId::Toml),
        _ => {}
    }
    language_for_alias(path.extension()?.to_str()?)
}

fn language_for_shebang(line: &str) -> Option<LanguageId> {
    let line = line.strip_prefix("#!")?.to_ascii_lowercase();
    if line.contains("python") {
        Some(LanguageId::Python)
    } else if line.contains("node") {
        Some(LanguageId::JavaScript)
    } else if line.contains("ruby") {
        Some(LanguageId::Ruby)
    } else if ["bash", "zsh", "/sh", " sh"]
        .iter()
        .any(|name| line.contains(name))
    {
        Some(LanguageId::Bash)
    } else {
        None
    }
}
