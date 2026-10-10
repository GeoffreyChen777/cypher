//! Highlight store (background, time-sliced, paint-only).

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Instant;

use cypher_syntax::LanguageId as Lang;
use gpui::{Context, SharedString, Task};

use crate::kit::syntax_cache::{DocumentHighlightKey, SyntaxHighlightCache};

use super::Transcript;

pub(super) struct HighlightEntry {
    key: DocumentHighlightKey,
    document: Option<Weak<cypher_syntax::HighlightedDocument>>,
    _task: Option<Task<()>>,
}

/// Cache of tokenized code blocks keyed by `(row id, block ix)`. Tokenization
/// runs on the background executor, time-sliced; results apply as paint-only
/// run colors when they land.
#[derive(Default)]
pub(super) struct HighlightStore {
    pub(super) entries: HashMap<(SharedString, usize), HighlightEntry>,
    cache: SyntaxHighlightCache,
}

impl HighlightStore {
    /// Current tokens if ready; kicks a background tokenize when stale/missing.
    pub(super) fn request(
        &mut self,
        row_id: SharedString,
        block_ix: usize,
        lang: Lang,
        code: &str,
        cx: &mut Context<Transcript>,
    ) -> Option<Arc<cypher_syntax::HighlightedDocument>> {
        let slot_key = (row_id.clone(), block_ix);
        let document_key = DocumentHighlightKey::new(lang, code);
        if let Some(entry) = self.entries.get(&slot_key)
            && entry.key == document_key
        {
            let document = entry.document.as_ref()?;
            if let Some(document) = document.upgrade() {
                return Some(document);
            }
        }
        if let Some(document) = self.cache.get(&document_key) {
            self.entries.insert(
                slot_key,
                HighlightEntry {
                    key: document_key,
                    document: Some(Arc::downgrade(&document)),
                    _task: None,
                },
            );
            return Some(document);
        }
        let code = code.to_string();
        let source_bytes = code.len();
        let task = cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let document = cx
                .background_executor()
                .spawn(async move {
                    cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                        source: &code,
                        path: None,
                        fence_tag: Some(match lang {
                            Lang::Rust => "rust",
                            Lang::JavaScript => "javascript",
                            Lang::Jsx => "jsx",
                            Lang::TypeScript => "typescript",
                            Lang::Tsx => "tsx",
                            Lang::Python => "python",
                            Lang::Go => "go",
                            Lang::Json => "json",
                            Lang::Jsonc => "jsonc",
                            Lang::Bash => "bash",
                            Lang::Toml => "toml",
                            Lang::Markdown => "markdown",
                            Lang::Html => "html",
                            Lang::Css => "css",
                            Lang::Yaml => "yaml",
                            Lang::C => "c",
                            Lang::Cpp => "cpp",
                            Lang::CSharp => "csharp",
                            Lang::Java => "java",
                            Lang::Kotlin => "kotlin",
                            Lang::Swift => "swift",
                            Lang::Ruby => "ruby",
                            Lang::Php => "php",
                            Lang::Sql => "sql",
                            Lang::Lua => "lua",
                            Lang::Dockerfile => "dockerfile",
                            Lang::Nix => "nix",
                            Lang::Make => "make",
                        }),
                    })
                    .ok()
                })
                .await;
            this.update(cx, |transcript, cx| {
                if let Some(document) = document {
                    let document = Arc::new(document);
                    let retained = transcript
                        .highlights
                        .cache
                        .insert(document_key, document.clone());
                    if let Some(entry) = transcript.highlights.entries.get_mut(&slot_key)
                        && entry.key == document_key
                    {
                        tracing::debug!(
                            language = ?lang,
                            source_bytes,
                            spans = document.lines.iter().map(Vec::len).sum::<usize>(),
                            elapsed_us = started.elapsed().as_micros() as u64,
                            "syntax highlight ready"
                        );
                        entry.document = retained.then(|| Arc::downgrade(&document));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.entries.insert(
            (row_id, block_ix),
            HighlightEntry {
                key: document_key,
                document: None,
                _task: Some(task),
            },
        );
        None
    }
}
