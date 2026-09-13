# Overall and region colors

Open **Settings → Appearance**. All choices are client-local, independent of the
settings device selector. They do not edit remote engines, terminal sessions,
repository files, message contents, or provider configuration.

## Inheritance

1. **Appearance mode** (System / Light / Dark) chooses the active appearance.
2. **Color theme** chooses Default, Catppuccin, Nord, or Gruvbox Soft for each
   appearance. Settings panels, sidebar project cards and Chat share the same
   default background. Bubbles, selections and raised controls use secondary tones.
3. **Chat**, **Terminal**, **Git / Diff**, and **Sidebar** can override their own
   colors. Existing Chat customization is retained.

Changing an overall preset changes only the baseline. It never clears overrides.
An empty field inherits from that baseline; a filled `#RRGGBB` field overrides it.
Switching appearances preserves both palettes.

The outer window backing and left-sidebar backing are fixed: color themes
do not recolor them or change their opacity. They retain the original
platform material for the active light/dark appearance. The sidebar itself is
transparent over the window frost; it does not add another tinted layer.

Each region can be expanded independently. Its palette selector identifies which
appearance is being edited. The region header's **Reset** clears only that
region's overrides in the selected palette. A field's **Reset** clears only that
field. **Reset** in Color theme changes only the selected overall preset back to
Default; it does not clear any region or Chat customization.

Click a color swatch to open the lightweight RGB picker. It includes common
colors, a saturation/brightness square, a hue strip and HEX input. Dragging
previews locally in the popup; releasing applies through the same validated
settings input. Palette clicks and valid HEX edits apply immediately. Esc or a
click outside closes the popup; an unfinished drag is discarded. No alpha
control is exposed, so the fixed window/sidebar material remains unaffected.

## Palette sources

These are workbench adaptations of established palettes, not full imports of
their syntax-highlighting or terminal themes. Existing single-color overrides
remain authoritative. Inline code uses the palette's body text over its
secondary surface for readable, restrained contrast.

| Theme | Dark background / text | Light background / text |
| --- | --- | --- |
| Catppuccin (Mocha / Latte) | `#1E1E2E` / `#CDD6F4` | `#EFF1F5` / `#4C4F69` |
| Nord | `#2E3440` / `#D8DEE9` | `#ECEFF4` / `#2E3440` |
| Gruvbox Soft | `#32302F` / `#EBDBB2` | `#F7F5EF` / `#3C3836` |

- [Catppuccin official palette](https://catppuccin.com/palette/):
  Base/Text, Mauve accent, Surface0/Mantle for secondary surfaces.
- [Nord official colors](https://www.nordtheme.com/docs/colors-and-palettes/):
  Polar Night/Snow Storm bases; nord8 accent in dark mode and nord3 in light
  mode to keep links readable instead of using pale Frost on a light background.
- [Gruvbox official implementation](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim):
  soft dark bg0, fg1 and mode-relative blue accent. The light surfaces are
  adapted to warm paper (`#F7F5EF`) and a subdued secondary beige (`#EEEAE1`)
  instead of the original strongly yellow backgrounds.

Saved `ocean`, `forest` and `warm` preset identifiers load as Catppuccin, Nord
and Gruvbox Soft respectively, without clearing any individual color overrides.

## Controls

### Terminal

- Background and default text.
- Cursor color, rendered as a 55% tint so the underlying character remains visible.
- Selection color, rendered as a 25% tint.
- **Advanced: ANSI 16 colors** exposes normal and bright ANSI slots 0–15.

Both the bottom terminal drawer and right-pane terminal use these choices.
Programs that explicitly emit RGB colors or extended indexed colors still control
those colors. The existing 256-color cube and appearance-aware grayscale behavior
are unchanged. Default foreground/background and reverse-video colors use the
configured terminal palette.

The preview uses the terminal's actual color resolver; it never executes commands.

### Git / Diff

- Background, default text and line numbers.
- Added-line and deleted-line backgrounds.

The right-pane Git view, history view and diff rows use these choices. Default
text also controls neutral syntax tokens such as variables and punctuation;
colored keywords, strings, comments and other semantic highlights are retained.
Addition/deletion markers keep their semantic green/red colors.

The preview uses the real diff-row renderer, without loading a repository.
This does not recolor ordinary Chat code blocks.

### Sidebar

- Project-card background (the sidebar backing is fixed).
- Primary and secondary text colors.
- Selected-row and hovered-row backgrounds.

The preview shows a project card, a selected conversation and a hoverable row.
Secondary UI labels may additionally use reduced opacity to preserve hierarchy.
Sidebar overrides do not change Chat, Git, Terminal or settings-form text.

## Validation and rendering

Valid edits save and repaint immediately. Incomplete or invalid HEX input remains
a draft and is not applied. A failed save leaves the previous live configuration
in place and displays an error.

Low-contrast combinations are warned about, not silently changed. ANSI black/dim
defaults are intentional; warnings cover explicitly overridden ANSI slots.
Explicit content/card background colors are opaque. The outer window and sidebar
backing remain unchanged regardless of the preset. A legacy `sidebarBackground`
override is ignored; the remaining overrides are preserved.

Backgrounds are painted by the rounded surface owners. Transcript and embedded
terminal viewports do not paint rectangular fills over their parent corners.

## Storage

- Overall theme and Terminal/Git/Sidebar overrides:
  `<client-data-dir>/appearance-colors.json`.
- Existing Chat typography and color overrides remain in
  `<client-data-dir>/chat-appearance.json`.
- The System/Light/Dark choice remains in `ui-settings.json`.

Color preference writes use a private temporary file, flush/sync and atomic
rename. They do not rewrite either of the other preference files. Missing or
invalid color files use defaults; malformed HEX values and unknown override keys
are ignored during normalization.
