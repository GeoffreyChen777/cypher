# Artifacts

An artifact is a page the agent writes for you to look at rather than read: an
explainer with a diagram, a small simulation, a chart, a UI mock-up. The
desktop chat shows it as a card; click the card to open the page in the
session's side panel.

## In the chat

- The agent writes the whole page in one fenced block tagged `artifact`:

  ````markdown
  ```html artifact title="Binary search, step by step"
  <!doctype html>
  …
  ```
  ````

  `svg artifact` holds a standalone SVG. Without a language the page is HTML;
  a language that doesn't render (`python artifact`) stays an ordinary code
  block. The title is optional. Without one the card uses the page's
  `<title>`, and failing that "HTML page" or "SVG image".
- The card replaces the code in the chat. While the fence is still streaming
  it reads "Writing…" and does not open; once the fence closes (or the reply
  ends) it opens on click.
- Plain ```` ```html ```` and ```` ```svg ```` fences keep their code and get
  a **Preview** button beside Copy that opens them the same way.
- Side chats open their artifacts in the same side panel.
- Other Markdown readers show the fence as an ordinary `html` code block.

## The Artifact surface

Each page opens as its own surface in the right dock (the `</>` icon on the
rail). Opening the card again brings the tab forward. A **named** artifact
(one with `title=`) that the agent writes again under the same title is a
revision: it reloads the open tab instead of adding one. Unnamed fences each
get their own tab.

- **Preview / Source**: the page in a web view, or its syntax-colored source.
- **Copy** copies the source.
- **Open in browser** writes the page to a private temp folder
  (`$TMPDIR/cypher-artifacts/`, named by its content) and opens it with the
  system's default app.

Pages load as text with no base URL. They can't read local files or the
project, but scripts, stylesheets and images from the web load, and clicked
web links open in the browser. A page renders on white unless it sets its own
background.

The web view is macOS only. On Linux the surface shows the source, and Open
in browser shows the page.

## On the iPhone

The iPhone app reads artifact fences the same way and shows the same card. A
tap opens the page full screen: Close on the left, and on the right a menu to
switch between Preview and Source and to copy the source. Its "Writing…" state
works the same way as the desktop's. Plain ```` ```html ```` and
```` ```svg ```` fences get a Preview button in their header.

The page runs in a web view with a non-persistent store, so nothing it saves
outlives the viewer. As on the desktop, it loads with no base URL, scripts and
styles from the web load, and tapped web links open in Safari. The page stays
loaded while you look at its source. There's no side panel and no reload in
place: a revised artifact is a new card to tap.

The demo fixture is `-demo -route chat:chat-artifacts`. Add `-sheet artifact`
(or `-sheet artifact-source`) to open the first card that shows.

## How the agent knows

The Pi runtime's `extensions/cypher-artifacts.ts` adds an `<artifacts>`
section to every turn's system prompt. It describes the fence, asks for
self-contained pages, and says to revise a page under the same title. Pi
renders the section after its built-in ones on every model; on Claude,
pi-claude-bridge forwards it with Pi's other custom sections. The engine
registers the extension once the device's runtime carries it, so a device on
an older runtime renders artifacts but its agent isn't told about them. You
can still ask for one ("show it as an `html artifact`").

## Trying it without a model

`scripts/dev-demo.sh --artifacts` opens a demo app with "Explain binary
search" under "Artifacts demo". Its first answer holds an interactive HTML
artifact, an SVG artifact and a plain HTML fence. Its second round revises the
HTML page under the same title, so the page's open tab reloads. Add `--slow` to
watch a card read "Writing…" while its fence streams.

In any development engine, the mock harness answers a prompt containing
`[mock:artifacts]` with the first answer and `[mock:artifacts:revise]` with
the revision.
