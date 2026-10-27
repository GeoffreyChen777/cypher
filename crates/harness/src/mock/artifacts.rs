//! Artifact demo replies for the mock harness: an interactive HTML artifact,
//! an SVG artifact and a plain ```` ```html ```` fence, then a revision of the
//! HTML artifact under the same title, so the chat's artifact cards, the HTML
//! fence's Preview button and the side panel's viewer (reloading on a
//! revision) can be seen without a real model.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the artifact showcase.
pub const MARKER: &str = "[mock:artifacts]";
/// Prompt marker: reply with the showcase's HTML artifact revised.
pub const REVISE_MARKER: &str = "[mock:artifacts:revise]";

/// The revision's edits to the showcase page: a comparison counter.
const REVISIONS: &[(&str, &str)] = &[
    (
        "each comparison discards half of what is left.</p>",
        "each comparison discards half of what is left. Sixteen numbers never take more than four (log₂ 16).</p>",
    ),
    (
        "<div class=\"cells\" id=\"cells\"></div>",
        "<p id=\"count\" class=\"lead\"></p>\n<div class=\"cells\" id=\"cells\"></div>",
    ),
    (
        "  let lo, hi, done, steps;\n",
        "  let lo, hi, done, steps;\n  const count = document.getElementById(\"count\");\n  const counted = () => { count.textContent = `${steps} of at most 4 comparisons`; };\n",
    ),
    (
        "done = false; steps = 0; log.textContent = \"\";",
        "done = false; steps = 0; log.textContent = \"\"; counted();",
    ),
    ("    steps++;\n", "    steps++;\n    counted();\n"),
];

/// One reply section per entry; each streams as its own delta, so the
/// artifact's fence arrives in pieces and its card reads "Writing…" first.
const SECTIONS: &[&str] = &[
    r##"Binary search halves the range it still has to look at on every step, so a sorted list of a million numbers takes at most 20 comparisons. Step through it here:

"##,
    r##"```html artifact title="Binary search, step by step"
<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Binary search, step by step</title>
<style>
  :root { color-scheme: light dark; font-family: -apple-system, system-ui, sans-serif; }
  body { margin: 0; padding: 24px; background: Canvas; color: CanvasText; }
  h1 { font-size: 18px; margin: 0 0 4px; }
  p.lead { margin: 0 0 18px; opacity: .7; font-size: 13px; }
  .cells { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 18px; }
"##,
    r##"  .cell { width: 40px; height: 40px; border-radius: 8px; display: grid; place-items: center;
          font: 600 14px ui-monospace, monospace; border: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
          transition: background .2s, opacity .2s, transform .2s; }
  .cell.out { opacity: .25; }
  .cell.mid { background: #6366f1; color: white; transform: translateY(-3px); }
  .cell.found { background: #16a34a; color: white; }
  .controls { display: flex; gap: 8px; align-items: center; margin-bottom: 14px; }
  input { width: 70px; padding: 6px 8px; border-radius: 6px; border: 1px solid #8884; font-size: 14px; }
  button { padding: 7px 14px; border-radius: 6px; border: 0; background: #6366f1; color: white; font-weight: 600; cursor: pointer; }
  button.ghost { background: transparent; color: inherit; border: 1px solid #8886; }
  #log { font: 13px ui-monospace, monospace; line-height: 1.7; white-space: pre-wrap; }
</style>
</head>
"##,
    r##"<body>
<h1>Binary search</h1>
<p class="lead">Pick a target, then step: each comparison discards half of what is left.</p>
<div class="controls">
  <label>Target <input id="target" type="number" value="37"></label>
  <button id="step">Step</button>
  <button id="reset" class="ghost">Reset</button>
</div>
<div class="cells" id="cells"></div>
<div id="log"></div>
<script>
  const values = [2, 5, 8, 12, 16, 23, 29, 37, 41, 48, 56, 63, 72, 81, 90, 99];
  const cells = document.getElementById("cells");
  const log = document.getElementById("log");
  let lo, hi, done, steps;
  function reset() {
    lo = 0; hi = values.length - 1; done = false; steps = 0; log.textContent = "";
    render(-1);
  }
"##,
    r##"  function render(mid, found) {
    cells.innerHTML = "";
    values.forEach((v, i) => {
      const el = document.createElement("div");
      el.className = "cell" + (i < lo || i > hi ? " out" : "") + (i === mid ? (found ? " found" : " mid") : "");
      el.textContent = v;
      cells.appendChild(el);
    });
  }
  function step() {
    if (done) return;
    const target = Number(document.getElementById("target").value);
    if (lo > hi) { log.textContent += `Range empty: ${target} is not in the list.\n`; done = true; return; }
    const mid = (lo + hi) >> 1;
    steps++;
    if (values[mid] === target) {
      log.textContent += `Step ${steps}: values[${mid}] = ${values[mid]} — found it.\n`;
      done = true; render(mid, true); return;
    }
    const lower = values[mid] < target;
    log.textContent += `Step ${steps}: values[${mid}] = ${values[mid]} ${lower ? "<" : ">"} ${target}, keep the ${lower ? "right" : "left"} half.\n`;
    if (lower) lo = mid + 1; else hi = mid - 1;
    render(mid);
  }
  document.getElementById("step").onclick = step;
  document.getElementById("reset").onclick = reset;
  document.getElementById("target").oninput = reset;
  reset();
</script>
</body>
</html>
```

"##,
    r##"The same idea as a picture — each level is one comparison, and the range shrinks by half:

```svg artifact title="Halving the range"
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 220" font-family="system-ui, sans-serif" font-size="12">
  <rect width="420" height="220" fill="#ffffff"/>
  <g fill="#6366f1" fill-opacity="0.85">
    <rect x="20" y="20" width="380" height="28" rx="6"/>
    <rect x="210" y="70" width="190" height="28" rx="6"/>
    <rect x="210" y="120" width="95" height="28" rx="6"/>
    <rect x="257" y="170" width="48" height="28" rx="6" fill="#16a34a"/>
  </g>
  <g fill="#ffffff" font-weight="600">
    <text x="30" y="39">16 candidates</text>
    <text x="220" y="89">8 left</text>
    <text x="220" y="139">4 left</text>
    <text x="262" y="189">2</text>
  </g>
  <g fill="#555555">
    <text x="20" y="89">step 1: 37 &gt; 29</text>
    <text x="20" y="139">step 2: 37 &lt; 56</text>
    <text x="20" y="189">step 3: 37 = 37 ✓</text>
  </g>
</svg>
```

"##,
    r##"And a plain HTML snippet, which keeps its code but can still be previewed from the button beside Copy:

```html
<button style="padding:8px 16px;border-radius:8px;border:0;background:#6366f1;color:white;font:600 14px system-ui">
  Search
</button>
```
"##,
];

/// The showcase reply's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    deltas(SECTIONS.iter().map(|text| text.to_string()))
}

/// The revision reply: the showcase's HTML artifact again, under the same
/// title, with a comparison counter; it streams in pieces too.
pub(super) fn revision() -> Vec<AgentEvent> {
    let page = revised_page();
    let pieces = page.split_inclusive("</style>\n").map(str::to_string);
    deltas(
        std::iter::once(
            "Here it is with a counter, so you can see the four-comparison bound hold:\n\n".to_string(),
        )
        .chain(pieces)
        .chain(std::iter::once(
            "Try a number that isn't in the list, like 40: the range empties after four comparisons.\n"
                .to_string(),
        )),
    )
}

fn revised_page() -> String {
    REVISIONS
        .iter()
        .fold(SECTIONS[1..5].concat(), |page, (from, to)| {
            page.replace(from, to)
        })
}

fn deltas(texts: impl Iterator<Item = String>) -> Vec<AgentEvent> {
    texts.map(|text| AgentEvent::TextDelta { text }).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_fence_closes_and_the_artifacts_are_marked() {
        let text: String = super::SECTIONS.concat();
        assert_eq!(text.matches("```").count() % 2, 0);
        assert_eq!(text.matches(" artifact title=").count(), 2);
        // The HTML artifact streams over several deltas.
        assert!(!super::SECTIONS[1].trim_end().ends_with("```"));
    }

    #[test]
    fn the_revision_edits_the_same_page_under_its_title() {
        let page: String = super::SECTIONS[1..5].concat();
        assert!(page.starts_with("```html artifact title=\"Binary search, step by step\""));
        assert!(page.trim_end().ends_with("```"));
        for (from, _) in super::REVISIONS {
            assert_eq!(page.matches(from).count(), 1, "{from}");
        }
        let revised = super::revised_page();
        assert!(revised.starts_with("```html artifact title=\"Binary search, step by step\""));
        // Defined once, called on reset and on every step.
        assert!(revised.contains("const counted = () =>"));
        assert_eq!(revised.matches("counted();").count(), 2);
        assert!(super::revision().len() > 3);
    }
}
