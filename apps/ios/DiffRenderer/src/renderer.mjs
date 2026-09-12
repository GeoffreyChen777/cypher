import { VirtualizedFile, VirtualizedFileDiff, Virtualizer, parsePatchFiles, registerCustomTheme } from "@pierre/diffs";
import { WorkerPoolManager } from "@pierre/diffs/worker";
import { inspectPatch, inspectSource, languageForPath, validateContext } from "./policy.mjs";
import { icon, notice, fileState, countText, theme, diffCSS, sourceCSS, languageLabel } from "./presentation.mjs";

registerCustomTheme("cypher-light", () => Promise.resolve(theme(false)));
registerCustomTheme("cypher-dark", () => Promise.resolve(theme(true)));
const themes = { light: "cypher-light", dark: "cypher-dark" };

const root = document.getElementById("diff");
let virtualizer, pool, workerURL, highlightDeadline;
let cards = [], retainedSourceBytes = 0;
let sourceRecord;
let requestCounter = 0;
const requests = new Map();
let generation = 0;
let state = { status: "ready" };
const notify = (event, id, fields = {}) => window.webkit?.messageHandlers.diffRenderer.postMessage({ event, id, ...fields });

function dispose() {
  generation++;
  clearTimeout(highlightDeadline);
  highlightDeadline = undefined;
  for (const card of cards) releaseCard(card);
  cards = [];
  sourceRecord = undefined;
  for (const request of requests.values()) {
    clearTimeout(request.timer);
    request.reject(new Error("cancelled"));
  }
  requests.clear();
  virtualizer?.cleanUp();
  pool?.terminate();
  if (workerURL) URL.revokeObjectURL(workerURL);
  virtualizer = pool = workerURL = undefined;
  retainedSourceBytes = 0;
  root.replaceChildren();
  state = { status: "disposed" };
}

function releaseCard(card) {
  card.epoch++;
  card.view?.cleanUp();
  card.view = undefined;
  card.file = undefined;
  card.retry = undefined;
  card.loading = false;
  retainedSourceBytes -= card.sourceBytes;
  card.sourceBytes = 0;
  card.body.replaceChildren();
}

function contextResult(value) {
  const request = requests.get(value.token);
  if (!request || value.id !== state.id) return;
  requests.delete(value.token);
  clearTimeout(request.timer);
  if (value.error) request.reject(new Error(value.error));
  else request.resolve(value);
}

async function loadContext(card, file, input) {
  const ticket = generation, epoch = card.epoch;
  notice(card.notice, "Loading context…", "loading");
  try {
    const token = `${generation}:${++requestCounter}`;
    const data = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        requests.delete(token);
        reject(new Error("unavailable"));
      }, 15_000);
      requests.set(token, { resolve, reject, timer });
      notify("context", input.id, { token, file: String(card.index) });
    });
    if (ticket !== generation || epoch !== card.epoch) throw new Error("cancelled");
    const texts = [data.oldText, data.newText];
    if (texts.some(text => text !== null && typeof text !== "string")) throw new Error("unavailable");
    const bytes = texts.reduce((sum, text) => sum + new TextEncoder().encode(text ?? "").length, 0);
    if (bytes > 512 * 1024 || texts.some(text => (text ?? "").split("\n").length > 10_000)) throw new Error("unavailable");
    if (retainedSourceBytes + bytes > 4 * 1024 * 1024) throw new Error("budget");
    validateContext(file, data.oldText, data.newText);
    if (bytes > 128 * 1024 || texts.some(text => (text ?? "").split("\n").some(line => line.length > 2_000))) {
      file.lang = "text";
    }
    retainedSourceBytes += bytes;
    card.sourceBytes += bytes;
    card.notice.replaceChildren();
    return {
      oldFile: file.type === "rename-pure" ? null : {
        name: file.prevName ?? file.name, contents: data.oldText ?? "",
        lang: file.lang, cacheKey: `${token}:old`,
      },
      newFile: { name: file.name, contents: data.newText ?? "", lang: file.lang, cacheKey: `${token}:new` },
    };
  } catch (error) {
    if (ticket === generation && epoch === card.epoch) {
      notice(card.notice, error.message === "stale" ? "Changes updated. Refresh to expand."
        : error.message === "budget" ? "Close another file to expand." : "Context unavailable",
        "warning", error.message === "stale" ? undefined : {
          title: "Retry", run: () => card.retry ? card.retry() : card.view?.loadFilesIfNecessary?.(),
        });
    }
    throw error;
  }
}

function mountCard(card, input, options) {
  if (card.entry.binary) {
    fileState(card.body, "Binary file", "Text preview isn’t available.", "binary");
    return;
  }
  try {
    const { plain } = inspectPatch(card.entry.patch);
    const parsed = parsePatchFiles(card.entry.patch, `${input.id}:${card.index}:${card.epoch}`, true)
      .flatMap(patch => patch.files);
    if (parsed.length !== 1) throw new Error("invalid");
    const file = card.file = parsed[0];
    file.lang = plain ? "text" : languageForPath(card.entry.path);
    file.cacheKey = `${input.id}:${card.index}:${card.epoch}`;
    if (file.prevName && file.prevName !== file.name) {
      const renamed = document.createElement("div");
      renamed.className = "file-note";
      renamed.textContent = `Renamed from ${file.prevName}`;
      card.body.append(renamed);
    }
    if (file.hunks.length === 0) {
      if (input.canLoadContext && ["rename-pure", "change"].includes(file.type)) {
        card.retry = async () => {
          if (card.loading) return;
          card.loading = true;
          const ticket = generation, epoch = card.epoch;
          try {
            const sources = await loadContext(card, file, input);
            if (ticket !== generation || epoch !== card.epoch) return;
            const container = document.createElement("diffs-container");
            card.body.append(container);
            // Unlike VirtualizedFileDiff, File requires complete metrics.
            card.view = new VirtualizedFile(options, virtualizer, undefined, pool);
            card.view.render({ file: sources.newFile, fileContainer: container });
          } catch {
            if (ticket === generation && epoch === card.epoch && !card.notice.textContent) {
              notice(card.notice, "Code unavailable", "warning");
            }
          } finally {
            if (ticket === generation && epoch === card.epoch) card.loading = false;
          }
        };
        notice(card.notice, "No text changes", "info", { title: "Show code", run: card.retry });
      } else {
        fileState(card.body, "No text changes", "This file has no changed lines.");
      }
      return;
    }
    if (input.split) {
      const columns = document.createElement("div");
      columns.className = "column-labels";
      for (const title of ["Before", "After"]) {
        const label = document.createElement("span");
        label.textContent = title;
        columns.append(label);
      }
      card.body.append(columns);
    }
    const container = document.createElement("diffs-container");
    card.body.append(container);
    card.view = new VirtualizedFileDiff({
      ...options,
      loadDiffFiles: input.canLoadContext ? diff => loadContext(card, diff, input) : undefined,
    }, virtualizer, { lineHeight: 20 }, pool);
    card.view.render({ fileDiff: file, fileContainer: container });
  } catch (error) {
    fileState(card.body, error.message === "large" ? "Diff too large" : "Diff unavailable",
      "Open Raw patch from the menu to inspect this snapshot.");
  }
}

function mountSource(input, options, policy) {
  const lang = policy.plain ? "text" : languageForPath(input.path);
  const summary = document.createElement("div");
  summary.className = "source-summary";
  const location = document.createElement("span");
  location.className = "source-location";
  location.textContent = input.path.includes("/") ? input.path.slice(0, input.path.lastIndexOf("/")) : "Project root";
  location.title = input.path;
  const metadata = document.createElement("span");
  metadata.className = "source-metadata";
  metadata.textContent = `${languageLabel(lang)} · ${policy.lines.toLocaleString("en")} ${policy.lines === 1 ? "line" : "lines"}`;
  summary.append(icon("file", "file-symbol"), location, metadata);
  root.append(summary);
  const body = document.createElement("div");
  body.className = "source-body";
  root.append(body);
  const sourceOptions = { ...options, overflow: input.wrap ? "wrap" : "scroll", unsafeCSS: diffCSS + sourceCSS };
  const view = new VirtualizedFile(sourceOptions, virtualizer, undefined, pool);
  const card = { body, view, input, options: sourceOptions, epoch: 0, sourceBytes: policy.bytes };
  cards.push(card);
  sourceRecord = card;
  retainedSourceBytes = policy.bytes;
  if (!policy.lines) { fileState(body, "Empty file", "This file has no text."); return; }
  const container = document.createElement("diffs-container");
  body.append(container);
  view.render({ file: { name: input.path, contents: input.sourceText, lang, cacheKey: input.id },
    fileContainer: container });
}

// Layout/appearance changes reuse the source instance, source data and worker.
// Pin a logical line rather than resetting a reader halfway through a file.
async function updateSource(input) {
  const card = sourceRecord, ticket = generation;
  const y = window.scrollY;
  const anchor = card.view.getNumericScrollAnchor(Math.max(0, y - (card.view.top ?? 0)));
  const offset = anchor ? (card.view.top ?? 0) + anchor.top - y : 0;
  const reflow = card.input.wrap !== input.wrap;
  card.input = input;
  card.options = { ...card.options, overflow: input.wrap ? "wrap" : "scroll",
    themeType: input.dark ? "dark" : "light" };
  state = { ...state, id: input.id, wrap: input.wrap };
  document.documentElement.style.colorScheme = input.dark ? "dark" : "light";
  card.view.setOptions(card.options);
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  if (ticket !== generation || card.input.id !== input.id) return;
  if (reflow && anchor && y > 0) {
    const position = card.view.getLinePosition(anchor.lineNumber);
    if (position) window.scrollTo(0, (card.view.top ?? 0) + position.top - offset);
  }
  notify("rendered", input.id);
}

function failWorker(ticket) {
  if (ticket !== generation) return;
  const id = state.id;
  dispose();
  state = { status: "error" };
  notify("error", id);
}

async function render(input) {
  if (sourceRecord && input.sourceText === sourceRecord.input.sourceText && input.path === sourceRecord.input.path) {
    return updateSource(input);
  }
  dispose();
  const ticket = generation;
  state = { status: "loading", id: input.id };
  try {
    const sourcePolicy = Object.hasOwn(input, "sourceText") ? inspectSource(input.sourceText) : undefined;
    const entries = sourcePolicy ? [] : input.files ?? [{ path: input.path, patch: input.patch }];
    if (!Array.isArray(entries)) throw new Error("invalid");
    // The existing one-file API remains useful for fixtures and error fallback.
    if (!input.files && !sourcePolicy) inspectPatch(input.patch);
    let totalBytes = 0, plain = sourcePolicy?.plain ?? false;
    const visibleEntries = entries.slice(0, 250).map(entry => {
      totalBytes += new TextEncoder().encode(entry.patch ?? "").length;
      if (totalBytes > 3 * 1024 * 1024) return { ...entry, patch: "" };
      try { plain ||= inspectPatch(entry.patch).plain; } catch { /* show per-file state */ }
      return entry;
    });
    document.documentElement.style.colorScheme = input.dark ? "dark" : "light";
    const style = input.split ? "split" : "unified";
    // Inline, locally bundled worker: no network fetch, CDN or file-origin
    // module worker dependency. One worker and two cached ASTs, not defaults 8/100.
    workerURL = URL.createObjectURL(new Blob([__WORKER_SOURCE__], { type: "text/javascript" }));
    pool = new WorkerPoolManager({
      workerFactory: () => new Worker(workerURL),
      poolSize: 1, totalASTLRUCacheSize: 2, workerInitializationTimeout: 8_000,
    }, {
      langs: [], theme: themes, preferredHighlighter: "shiki-js",
      lineDiffType: plain ? "none" : "word-alt",
      maxLineDiffLength: 500, tokenizeMaxLineLength: 1_000,
    });
    const currentPool = pool;
    await currentPool.initialize();
    if (ticket !== generation) return;
    if (!currentPool.isWorkingPool()) throw new Error("worker");
    virtualizer = new Virtualizer({ overscrollSize: 400, intersectionObserverMargin: 400 });
    virtualizer.setup(document);
    const options = {
      diffStyle: style,
      theme: themes, unsafeCSS: diffCSS,
      themeType: input.dark ? "dark" : "light",
      disableFileHeader: true, stickyHeader: false,
      diffIndicators: "bars", hunkSeparators: "line-info",
      expandUnchanged: false, collapsedContextThreshold: 6, expansionLineCount: 20,
      overflow: "scroll", edit: false, enableLineSelection: false,
      enableGutterUtility: false, lineHoverHighlight: "disabled",
      lineDiffType: plain ? "none" : "word-alt",
      maxLineDiffLength: 500, tokenizeMaxLineLength: 1_000,
      tokenizeMaxLength: 160_000, disableErrorHandling: true,
      onPostRender: node => {
        for (const label of node.shadowRoot?.querySelectorAll("[data-unmodified-lines]") ?? []) {
          const text = label.textContent;
          const compact = text === "More unchanged context may be available"
            ? "Show more context" : text.replace(/ unmodified line(s?)$/, " unchanged line$1");
          if (compact !== text) label.textContent = compact;
        }
        for (const button of node.shadowRoot?.querySelectorAll("[data-expand-button]") ?? []) {
          button.setAttribute("aria-label", button.hasAttribute("data-expand-all-button")
            ? "Expand all unchanged lines" : "Expand unchanged lines");
          button.tabIndex = 0;
          if (!button.dataset.cypherAccessible) {
            button.dataset.cypherAccessible = "true";
            button.addEventListener("keydown", event => {
              if (event.key === "Enter" || event.key === " ") { event.preventDefault(); button.click(); }
            });
          }
        }
      },
    };
    if (sourcePolicy) mountSource(input, options, sourcePolicy);
    const makeStats = (additions, deletions) => {
      const stats = document.createElement("span");
      stats.className = "file-stat";
      for (const [kind, value, sign] of [["added", additions, "+"], ["removed", deletions, "−"]]) {
        const span = document.createElement("span");
        span.className = kind + (value ? "" : " zero");
        span.textContent = sign + countText(value);
        stats.append(span);
      }
      return stats;
    };
    root.style.setProperty("--stat-width", Math.max(3, ...visibleEntries.flatMap(entry =>
      [entry.additions, entry.deletions].map(value => countText(value).length + 1))) + "ch");
    if (input.files) {
      const summary = document.createElement("div");
      summary.className = "changes-summary";
      const label = document.createElement("span");
      const total = entries.length + (input.omittedFiles ?? 0);
      label.textContent = total > visibleEntries.length
        ? `Showing ${visibleEntries.length} of ${total} files`
        : `${total} ${total === 1 ? "file" : "files"} changed`;
      summary.append(label, makeStats(entries.reduce((n, f) => n + (f.additions ?? 0), 0),
        entries.reduce((n, f) => n + (f.deletions ?? 0), 0)));
      root.append(summary);
    }
    for (const [index, entry] of visibleEntries.entries()) {
      const section = document.createElement("section");
      section.className = "file";
      const body = document.createElement("div");
      body.className = "file-body";
      const notice = document.createElement("div");
      notice.className = "context-notice";
      notice.setAttribute("role", "status");
      const card = { index, entry, body, notice, epoch: 0, sourceBytes: 0 };
      cards.push(card);
      if (input.files) {
        const header = document.createElement("button");
        header.className = "file-header";
        header.setAttribute("aria-label", entry.path);
        header.setAttribute("aria-expanded", "true");
        const name = document.createElement("span");
        name.className = "file-name";
        const title = document.createElement("span");
        title.className = "file-title";
        const slash = entry.path.lastIndexOf("/");
        const filename = entry.path.slice(slash + 1);
        const dot = filename.lastIndexOf(".");
        const hasExtension = dot > 0 && filename.length - dot <= 12;
        const stem = document.createElement("span");
        stem.className = "file-stem";
        stem.textContent = hasExtension ? filename.slice(0, dot) : filename;
        title.append(stem);
        if (hasExtension) {
          const extension = document.createElement("span");
          extension.className = "file-extension";
          extension.textContent = filename.slice(dot);
          title.append(extension);
        }
        name.append(title);
        if (slash >= 0) {
          const location = document.createElement("span");
          location.className = "file-location";
          location.textContent = entry.path.slice(0, slash);
          name.append(location);
        }
        header.append(icon(entry.binary ? "binary" : "file", "file-symbol"), name,
          makeStats(entry.additions, entry.deletions), icon("chevron", "file-disclosure"));
        header.onclick = () => {
          const expanded = header.getAttribute("aria-expanded") === "true";
          header.setAttribute("aria-expanded", String(!expanded));
          notice.replaceChildren();
          releaseCard(card);
          if (!expanded) mountCard(card, input, options);
        };
        section.append(header);
      }
      section.append(notice, body);
      root.append(section);
      mountCard(card, input, options);
    }
    if (entries.length > visibleEntries.length || input.omittedFiles > 0) {
      const limit = document.createElement("p");
      limit.textContent = "Showing first 250 files";
      root.append(limit);
    }
    state = { status: "rendered", id: input.id, style, plain, worker: true, path: input.path,
      mode: sourcePolicy ? "file" : "diff", wrap: input.wrap ?? false };
    currentPool.subscribeToStatChanges(stats => {
      if (ticket !== generation) return;
      if (stats.workersFailed) {
        failWorker(ticket);
        return;
      }
      const busy = stats.busyWorkers || stats.activeTasks || stats.queuedTasks;
      if (!busy) { clearTimeout(highlightDeadline); highlightDeadline = undefined; return; }
      if (highlightDeadline) return;
      highlightDeadline = setTimeout(() => {
        failWorker(ticket);
      }, 8_000);
    });
    window.scrollTo(0, 0);
    notify("rendered", input.id);
  } catch (error) {
    if (ticket !== generation) return;
    const kind = error.message === "large" ? "large" : "error";
    dispose();
    state = { status: kind };
    notify(kind, input.id);
  }
}

// The context bridge uses a native-owned file index and a pinned snapshot.
window.cypherDiff = Object.freeze({
  render, dispose, contextResult,
  inspect: () => ({ ...state, cachedDiffs: pool?.getStats().diffCacheSize ?? 0,
    cachedFiles: pool?.getStats().fileCacheSize ?? 0, generation, retainedSourceBytes }),
});
window.addEventListener("pagehide", dispose);
window.addEventListener("unhandledrejection", event => {
  event.preventDefault();
  // Do not log exceptions containing repository source text.
});
notify("ready", "");
