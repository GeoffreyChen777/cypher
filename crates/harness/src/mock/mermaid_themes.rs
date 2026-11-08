//! Mermaid theme gallery for the mock harness: the same flowchart and
//! sequence diagram under the app's own palette, each theme Mermaid 12 ships
//! (with its looks) and a few editor palettes written as `themeVariables`.
//! Every diagram names its theme in its own front matter, so the gallery
//! shows exactly what a model gets by writing that config.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the theme gallery.
pub const MARKER: &str = "[mock:mermaid-themes]";

const FLOW: &str = "flowchart LR
  subgraph App
    UI[Transcript] --> Composer
  end
  Composer -->|send| Engine{Needs tools?}
  Engine -->|yes| Tools[(Run tools)]
  Engine -->|no| Reply([Stream reply])
  Tools --> Reply";

const SEQUENCE: &str = "sequenceDiagram
  participant UI
  participant Engine
  participant Model
  UI->>Engine: send(prompt)
  Engine->>Model: complete
  Note over Engine,Model: tool loop
  Model-->>Engine: stream deltas
  Engine-->>UI: transcript update";

/// One gallery entry: its heading, a line about it, and the front matter
/// config (`None`: the app's own palette, no config at all).
struct Entry {
    title: &'static str,
    about: &'static str,
    config: Option<&'static str>,
}

/// An editor palette as Mermaid `base` theme variables.
macro_rules! palette {
    (dark: $dark:literal, bg: $bg:literal, node: $node:literal, alt: $alt:literal,
     deep: $deep:literal, text: $text:literal, border: $border:literal,
     muted: $muted:literal, line: $line:literal, note: $note:literal,
     note_text: $note_text:literal) => {
        concat!(
            "  theme: base\n",
            "  look: classic\n",
            "  darkMode: ",
            $dark,
            "\n",
            "  themeVariables:\n",
            "    background: '",
            $bg,
            "'\n",
            "    primaryColor: '",
            $node,
            "'\n",
            "    primaryTextColor: '",
            $text,
            "'\n",
            "    primaryBorderColor: '",
            $border,
            "'\n",
            "    secondaryColor: '",
            $alt,
            "'\n",
            "    tertiaryColor: '",
            $deep,
            "'\n",
            "    textColor: '",
            $text,
            "'\n",
            "    lineColor: '",
            $line,
            "'\n",
            "    edgeLabelBackground: '",
            $bg,
            "'\n",
            "    clusterBkg: '",
            $deep,
            "'\n",
            "    clusterBorder: '",
            $muted,
            "'\n",
            "    actorBkg: '",
            $node,
            "'\n",
            "    actorBorder: '",
            $border,
            "'\n",
            "    actorTextColor: '",
            $text,
            "'\n",
            "    actorLineColor: '",
            $muted,
            "'\n",
            "    signalColor: '",
            $line,
            "'\n",
            "    signalTextColor: '",
            $text,
            "'\n",
            "    noteBkgColor: '",
            $alt,
            "'\n",
            "    noteBorderColor: '",
            $note,
            "'\n",
            "    noteTextColor: '",
            $note_text,
            "'\n",
        )
    };
}

const ENTRIES: &[Entry] = &[
    Entry {
        title: "App palette (current)",
        about: "What every diagram gets today: colors from the chat theme, classic look.",
        config: None,
    },
    Entry {
        title: "App palette + neo look",
        about: "Our colors with Mermaid 12's neo shapes: rounded corners, soft shadows.",
        config: Some("  look: neo\n"),
    },
    Entry {
        title: "redux-color · neo",
        about: "Mermaid 12's default: a color per participant, entity and subgraph.",
        config: Some("  theme: redux-color\n  look: neo\n"),
    },
    Entry {
        title: "redux · neo",
        about: "The same shapes in monochrome.",
        config: Some("  theme: redux\n  look: neo\n"),
    },
    Entry {
        title: "redux-dark-color · neo",
        about: "The dark default (best seen in dark mode).",
        config: Some("  theme: redux-dark-color\n  look: neo\n"),
    },
    Entry {
        title: "redux-dark · neo",
        about: "Dark, monochrome.",
        config: Some("  theme: redux-dark\n  look: neo\n"),
    },
    Entry {
        title: "neo · neo",
        about: "Gradient borders.",
        config: Some("  theme: neo\n  look: neo\n"),
    },
    Entry {
        title: "neo-dark · neo",
        about: "Gradient borders on dark.",
        config: Some("  theme: neo-dark\n  look: neo\n"),
    },
    Entry {
        title: "neutral",
        about: "Greys, made for printing.",
        config: Some("  theme: neutral\n  look: classic\n"),
    },
    Entry {
        title: "default",
        about: "Classic Mermaid lavender.",
        config: Some("  theme: default\n  look: classic\n"),
    },
    Entry {
        title: "forest",
        about: "Classic Mermaid greens.",
        config: Some("  theme: forest\n  look: classic\n"),
    },
    Entry {
        title: "dark",
        about: "Classic Mermaid dark.",
        config: Some("  theme: dark\n  look: classic\n"),
    },
    Entry {
        title: "One Dark",
        about: "The Atom/Zed palette, blue arrows.",
        config: Some(
            palette!(dark: "true", bg: "#282c34", node: "#2c313a", alt: "#3a3f4b",
            deep: "#21252b", text: "#abb2bf", border: "#4b5263", muted: "#4b5263",
            line: "#61afef", note: "#e5c07b", note_text: "#e5c07b"),
        ),
    },
    Entry {
        title: "Catppuccin Mocha",
        about: "Soft pastels on dark.",
        config: Some(
            palette!(dark: "true", bg: "#1e1e2e", node: "#313244", alt: "#45475a",
            deep: "#181825", text: "#cdd6f4", border: "#cba6f7", muted: "#585b70",
            line: "#89b4fa", note: "#f9e2af", note_text: "#f9e2af"),
        ),
    },
    Entry {
        title: "Catppuccin Latte",
        about: "Catppuccin's light flavor.",
        config: Some(
            palette!(dark: "false", bg: "#eff1f5", node: "#e6e9ef", alt: "#ccd0da",
            deep: "#dce0e8", text: "#4c4f69", border: "#8839ef", muted: "#bcc0cc",
            line: "#1e66f5", note: "#df8e1d", note_text: "#4c4f69"),
        ),
    },
    Entry {
        title: "Nord",
        about: "Arctic blues.",
        config: Some(
            palette!(dark: "true", bg: "#2e3440", node: "#3b4252", alt: "#434c5e",
            deep: "#2e3440", text: "#eceff4", border: "#88c0d0", muted: "#4c566a",
            line: "#81a1c1", note: "#ebcb8b", note_text: "#ebcb8b"),
        ),
    },
    Entry {
        title: "Tokyo Night",
        about: "Deep navy with violet arrows.",
        config: Some(
            palette!(dark: "true", bg: "#1a1b26", node: "#24283b", alt: "#292e42",
            deep: "#16161e", text: "#c0caf5", border: "#7aa2f7", muted: "#3b4261",
            line: "#bb9af7", note: "#e0af68", note_text: "#e0af68"),
        ),
    },
    Entry {
        title: "Dracula",
        about: "Purple and pink.",
        config: Some(
            palette!(dark: "true", bg: "#282a36", node: "#44475a", alt: "#6272a4",
            deep: "#21222c", text: "#f8f8f2", border: "#bd93f9", muted: "#6272a4",
            line: "#ff79c6", note: "#f1fa8c", note_text: "#f1fa8c"),
        ),
    },
    Entry {
        title: "Gruvbox Dark",
        about: "Warm browns and yellow.",
        config: Some(
            palette!(dark: "true", bg: "#282828", node: "#3c3836", alt: "#504945",
            deep: "#32302f", text: "#ebdbb2", border: "#665c54", muted: "#665c54",
            line: "#fabd2f", note: "#b8bb26", note_text: "#ebdbb2"),
        ),
    },
    Entry {
        title: "GitHub light",
        about: "The palette GitHub READMEs often use.",
        config: Some(
            palette!(dark: "false", bg: "#ffffff", node: "#ddf1ff", alt: "#f6f8fa",
            deep: "#ffffff", text: "#24292f", border: "#0969da", muted: "#d0d7de",
            line: "#57606a", note: "#bf8700", note_text: "#24292f"),
        ),
    },
];

fn fence(config: Option<&str>, body: &str) -> String {
    match config {
        Some(config) => format!("```mermaid\n---\nconfig:\n{config}---\n{body}\n```\n"),
        None => format!("```mermaid\n{body}\n```\n"),
    }
}

/// The gallery as one Markdown reply.
pub fn text() -> String {
    let mut out = String::from(
        "The same two diagrams under every theme. A diagram picks its theme in its own \
         front matter (switch a block to Source to see it); dark themes read best with \
         the app in dark mode.\n",
    );
    for entry in ENTRIES {
        out.push_str(&format!("\n### {}\n\n{}\n\n", entry.title, entry.about));
        out.push_str(&fence(entry.config, FLOW));
        out.push('\n');
        out.push_str(&fence(entry.config, SEQUENCE));
    }
    out
}

/// Bytes per streamed delta: one diagram's worth, so the gallery streams in
/// quickly but block by block.
const CHUNK: usize = 400;

/// The gallery's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    let text = text();
    let mut deltas = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        deltas.push(AgentEvent::TextDelta {
            text: text[start..end].into(),
        });
        start = end;
    }
    deltas
}

#[cfg(test)]
mod tests {
    use cypher_proto::AgentEvent;

    #[test]
    fn every_theme_gets_both_diagrams() {
        let text: String = super::reply()
            .into_iter()
            .map(|event| match event {
                AgentEvent::TextDelta { text } => text,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(text, super::text());
        assert_eq!(text.matches("```mermaid").count(), super::ENTRIES.len() * 2);
        assert_eq!(
            text.matches("config:\n").count(),
            (super::ENTRIES.len() - 1) * 2
        );
        assert!(text.contains("theme: redux-color\n  look: neo\n---\nflowchart LR"));
        assert!(text.contains("    primaryColor: '#313244'\n"));
    }
}
