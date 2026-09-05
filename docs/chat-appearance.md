# Chat appearance

Open **Settings → Appearance → Chat interface**.

For coordinated app themes and independent Terminal, Git and Sidebar colors,
see [Overall and region colors](appearance-colors.md). Chat overrides take
precedence over the overall palette and are preserved when that palette changes.

These are **client-local preferences**, shared by this client's chats and Side
Chats. They do not follow the settings device selector and do not change the
sidebar, settings forms, terminal, remote engine, or stored message content.

## Controls

- **Message font:** choose from installed font families, with searchable
  selection and a system-font option. Default uses Geist.
- **Code font:** independent family selection; default uses Geist Mono.
- **Message font size:** 12–32 px, including the chat input.
- **Code font size:** 10–24 px in 0.5 px steps.
- **Line spacing:** 80–200% of the original line heights. At 100%, body text is
  14/22 px, code is 12.5/18 px, and the input is 14/22.75 px.
- **Paragraph spacing:** 0–40 px. **Message spacing:** 4–64 px.
- **Wide-screen mode:** removes the 736 px message / 768 px composer column
  caps. Normal chat gutters remain clear for the message rail; side panels
  keep their existing widths.
- **Colors:** edit light and dark overrides independently. Overall presets live
  in **Color theme**; Chat has individual `#RRGGBB` fields for message
  text, chat background, links/accent, user message bubbles, code block
  background, code block base text, inline code text, and inline code background.

Code block text controls plain code and neutral tokens (variables, parameters,
operators, and punctuation). Keywords, strings, comments, numbers, and other
colored syntax tokens retain their existing highlights. These overrides apply
only to fenced code blocks, not inline code, terminal output, or Diff panels.

**Inline code text** and **Inline code background** independently style Markdown
backtick spans, including those in lists, headings, and tables. They do not
recolor fenced code blocks, syntax highlighting, or file/session mention chips.
Clearing either field restores its original theme color or translucent wash.

The preview uses the real Markdown renderer, including headings, inline code,
links, and syntax-highlighted code blocks. Its light/dark selector previews the palette being
edited, which can differ from the app's current appearance.

Changes save and apply immediately. Empty color fields follow the normal
theme. Incomplete/invalid colors are not applied. Low-contrast combinations
are warned about, not silently changed. A custom background is opaque;
clearing it restores the existing glass treatment.

**Reset** in the Chat interface header restores typography, spacing and normal width, and clears
both Chat override palettes so they follow the overall theme. It does not reset
the overall preset, Terminal/Git/Sidebar overrides, or System/Light/Dark choice.

## Storage and fallback

Preferences live in `<client-data-dir>/chat-appearance.json`, separate from
`ui-settings.json`. This prevents a debounced pane/tab save from overwriting
chat appearance. Writes use a unique temporary file and atomic rename.

Missing preferences preserve the original look. Out-of-range values are
clamped, malformed colors fall back to the theme, and fonts unavailable on
this computer fall back to the default family. Installed fonts are discovered
when the app starts; restart after installing a new font.

Typography changes invalidate cached text runs and virtual-list measurements
without resetting transcript contents or folds. The input's shaping, caret,
selection, scrolling, and compact height use the same updated line metrics.
