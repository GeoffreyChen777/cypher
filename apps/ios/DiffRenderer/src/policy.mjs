export const MAX_BYTES = 512 * 1024;
export const MAX_LINES = 10_000;

export function inspectSource(text) {
  if (typeof text !== "string") throw new Error("invalid");
  const bytes = new TextEncoder().encode(text).length;
  const lines = text === "" ? [] : text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  if (bytes > MAX_BYTES || lines.length > MAX_LINES) throw new Error("large");
  return { bytes, lines: lines.length,
    plain: bytes > 128 * 1024 || lines.length > 4_000 || lines.some(line => line.length > 2_000) };
}

// Refuse oversized patches rather than cutting through a hunk and fabricating
// a valid-looking diff. The native raw-text fallback has its own preview limit.
export function inspectPatch(patch) {
  if (typeof patch !== "string" || !patch.startsWith("diff --git ")) throw new Error("invalid");
  if (new TextEncoder().encode(patch).length > MAX_BYTES) throw new Error("large");
  const lines = patch.split("\n");
  if (lines.length > MAX_LINES) throw new Error("large");
  let longest = 0;
  for (const line of lines) {
    longest = Math.max(longest, line.length);
    const hunk = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (hunk) {
      if (+hunk[1] > 2 ** 31 - 1 || +hunk[3] > 2 ** 31 - 1 ||
          +(hunk[2] ?? 1) > MAX_LINES || +(hunk[4] ?? 1) > MAX_LINES) throw new Error("large");
    }
  }
  return { plain: longest > 2_000 || lines.length > 4_000 };
}

export function languageForPath(path) {
  const ext = path.split(".").pop().toLowerCase();
  const languages = {
    swift: "swift", rs: "rust", ts: "typescript", tsx: "tsx",
    js: "javascript", jsx: "jsx", py: "python", go: "go",
    json: "json", css: "css", html: "html", sh: "shellscript",
    bash: "shellscript", yaml: "yaml", yml: "yaml", toml: "toml",
    c: "c", h: "c", cpp: "cpp", hpp: "cpp", m: "objective-c",
  };
  return Object.hasOwn(languages, ext) ? languages[ext] : "text";
}

// Check both the visible hunks and the supposedly unchanged gaps before
// allowing Pierre to hydrate. Its hydration API does not itself verify that
// the supplied documents still describe the patch.
export function validateContext(file, oldText, newText) {
  if (typeof newText !== "string" || (file.type !== "rename-pure" && typeof oldText !== "string")) {
    throw new Error("unavailable");
  }
  const lines = text => {
    if (!text) return [];
    const result = text.replace(/\r\n/g, "\n").split("\n");
    if (result.at(-1) === "") result.pop();
    return result;
  };
  const old = lines(oldText), next = lines(newText);
  const normalize = line => line?.replace(/\r?\n$/, "");
  const equal = (a, ai, ac, b, bi, bc) => {
    if (ac < 0 || bc < 0 || ac !== bc || ai + ac > a.length || bi + bc > b.length) throw new Error("stale");
    for (let i = 0; i < ac; i++) if (normalize(a[ai + i]) !== normalize(b[bi + i])) throw new Error("stale");
  };
  if (file.type === "rename-pure") {
    if (oldText !== null) equal(old, 0, old.length, next, 0, next.length);
    return;
  }
  let oi = 0, ni = 0;
  for (const hunk of file.hunks) {
    const os = hunk.deletionCount === 0 ? hunk.deletionStart : Math.max(0, hunk.deletionStart - 1);
    const ns = hunk.additionCount === 0 ? hunk.additionStart : Math.max(0, hunk.additionStart - 1);
    equal(old, oi, os - oi, next, ni, ns - ni);
    equal(old, os, hunk.deletionCount, file.deletionLines, hunk.deletionLineIndex, hunk.deletionCount);
    equal(next, ns, hunk.additionCount, file.additionLines, hunk.additionLineIndex, hunk.additionCount);
    oi = os + hunk.deletionCount;
    ni = ns + hunk.additionCount;
  }
  equal(old, oi, old.length - oi, next, ni, next.length - ni);
}
