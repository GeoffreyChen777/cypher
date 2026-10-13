//! Diff fence demo reply for the mock harness: the shapes of ```` ```diff ````
//! a model writes in chat, so the transcript's diff fence renderer can be
//! seen without a real model.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the diff fence showcase.
pub const MARKER: &str = "[mock:diffs]";

/// One reply section per entry; each streams as its own delta.
const SECTIONS: &[&str] = &[
    r##"Here are the changes, from the loosest chat-style diff to a full git patch.

"##,
    r##"**Chat-style diff** — no file header, `@@` labels without line ranges:

```diff
     fingerprint: dict[str, Any] = {
         "type": _type_identity(tokenizer),
-        "name_or_path": getattr(tokenizer, "name_or_path", None),
         "chat_template": getattr(tokenizer, "chat_template", None),
@@ slow path
         init_kwargs = getattr(tokenizer, "init_kwargs", None)
+        if isinstance(init_kwargs, dict):
+            # Resource paths only; file contents are hashed via vocab/bpe_ranks/sp_models.
+            paths = {"name_or_path", "tokenizer_file", "special_tokens_map_file", *getattr(tokenizer, "vocab_files_names", {})}
+            init_kwargs = {k: v for k, v in init_kwargs.items() if k not in paths}
@@ def processor_fingerprint
-    payload["scalars"] = _scalar_attributes(processor)
     tokenizer = getattr(processor, "tokenizer", None)
     if tokenizer is None and (...):
         tokenizer = processor
+    # A bare tokenizer is covered by its content fingerprint; its scalars hold load paths.
+    payload["scalars"] = {} if tokenizer is processor else _scalar_attributes(processor)
```

"##,
    r##"**Same change as a git patch** — the path colors the code and the ranges number the lines:

```diff
diff --git a/src/fingerprint.py b/src/fingerprint.py
index 3f2a1c4..9b7e0d2 100644
--- a/src/fingerprint.py
+++ b/src/fingerprint.py
@@ -41,7 +41,6 @@ def tokenizer_fingerprint(tokenizer) -> str:
     fingerprint: dict[str, Any] = {
         "type": _type_identity(tokenizer),
-        "name_or_path": getattr(tokenizer, "name_or_path", None),
         "chat_template": getattr(tokenizer, "chat_template", None),
     }
     if _has_fast_path(tokenizer):
@@ -58,6 +57,10 @@ def tokenizer_fingerprint(tokenizer) -> str:
         return _digest(fingerprint)
     init_kwargs = getattr(tokenizer, "init_kwargs", None)
+    if isinstance(init_kwargs, dict):
+        # Resource paths only; file contents are hashed via vocab/bpe_ranks/sp_models.
+        paths = {"name_or_path", "tokenizer_file", "special_tokens_map_file"}
+        init_kwargs = {k: v for k, v in init_kwargs.items() if k not in paths}
     fingerprint["init_kwargs"] = init_kwargs
     return _digest(fingerprint)
```

"##,
    r##"**Several files at once**, including a new file and a missing final newline:

```patch
--- a/crates/ui/src/theme.rs
+++ b/crates/ui/src/theme.rs
@@ -12,4 +12,5 @@ pub struct Theme {
     pub text: Hsla,
     pub text_muted: Hsla,
+    pub diff_hunk_bg: Hsla,
     pub border: Hsla,
 }
--- /dev/null
+++ b/docs/diffs.md
@@ -0,0 +1,3 @@
+# Diffs
+
+Fenced `diff` blocks render with the Changes pane's rows.
--- a/package.json
+++ b/package.json
@@ -1,3 +1,3 @@
 {
-  "version": "0.3.49"
+  "version": "0.3.50"
 }
\ No newline at end of file
```

"##,
    r##"Long lines scroll sideways inside the block, and the copy button copies the raw diff."##,
];

/// The showcase reply's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    SECTIONS
        .iter()
        .map(|text| AgentEvent::TextDelta {
            text: (*text).into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_fence_closes() {
        let text: String = super::SECTIONS.concat();
        assert_eq!(text.matches("```").count() % 2, 0);
        assert!(text.contains("\n     fingerprint: dict"));
    }
}
