// A bounded offline language registry. Core does not import Shiki's full
// language/theme bundles. Unsupported filenames deliberately use plain text.
export * from "shiki/core";
export { createHighlighterCore as createHighlighter } from "shiki/core";
export { createJavaScriptRegexEngine } from "shiki/engine/javascript";
export { createOnigurumaEngine } from "shiki/engine/oniguruma";
// Pierre re-exports this helper; it is unused and tree-shaken from our entry.
export { codeToHtml } from "shiki/bundle/web";

export const bundledLanguages = {
  swift: () => import("@shikijs/langs/swift"),
  rust: () => import("@shikijs/langs/rust"),
  typescript: () => import("@shikijs/langs/typescript"),
  tsx: () => import("@shikijs/langs/tsx"),
  javascript: () => import("@shikijs/langs/javascript"),
  jsx: () => import("@shikijs/langs/jsx"),
  python: () => import("@shikijs/langs/python"),
  go: () => import("@shikijs/langs/go"),
  json: () => import("@shikijs/langs/json"),
  css: () => import("@shikijs/langs/css"),
  html: () => import("@shikijs/langs/html"),
  shellscript: () => import("@shikijs/langs/shellscript"),
  yaml: () => import("@shikijs/langs/yaml"),
  toml: () => import("@shikijs/langs/toml"),
  c: () => import("@shikijs/langs/c"),
  cpp: () => import("@shikijs/langs/cpp"),
  "objective-c": () => import("@shikijs/langs/objective-c"),
};
