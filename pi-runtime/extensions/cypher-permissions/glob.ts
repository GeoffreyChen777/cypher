/**
 * Path globs for zone patterns: `**` crosses directories, `*` and `?` stay
 * inside one component, `[...]` is a character class. A pattern ending in
 * `/**` also matches the directory itself, so `~/.ssh/**` covers `ls ~/.ssh`.
 *
 * macOS volumes are case-insensitive by default, so there the match ignores
 * case: `~/.SSH/config` is the same file as `~/.ssh/config`.
 */

const CASE_INSENSITIVE = process.platform === "darwin";

function escapeRegExp(char: string): string {
  return /[\\^$.*+?()[\]{}|]/.test(char) ? `\\${char}` : char;
}

export function globToRegExp(pattern: string, caseInsensitive = CASE_INSENSITIVE): RegExp {
  let source = "^";
  let index = 0;
  while (index < pattern.length) {
    const char = pattern[index];
    if (char === "*") {
      if (pattern[index + 1] === "*") {
        const slashAfter = pattern[index + 2] === "/";
        const atStart = index === 0 || pattern[index - 1] === "/";
        if (atStart && slashAfter) {
          // `**/` matches zero or more whole directories.
          source += "(?:.*/)?";
          index += 3;
          continue;
        }
        if (atStart && index + 2 === pattern.length) {
          // A trailing `/**` also matches the directory itself.
          if (source.endsWith("/")) source = `${source.slice(0, -1)}(?:/.*)?`;
          else source += ".*";
          index += 2;
          continue;
        }
        source += ".*";
        index += 2;
        continue;
      }
      source += "[^/]*";
      index += 1;
      continue;
    }
    if (char === "?") {
      source += "[^/]";
      index += 1;
      continue;
    }
    if (char === "[") {
      const end = pattern.indexOf("]", index + 2);
      if (end > index) {
        let body = pattern.slice(index + 1, end).replace(/\\/g, "\\\\");
        if (body.startsWith("!")) body = `^${body.slice(1)}`;
        source += `[${body}]`;
        index = end + 1;
        continue;
      }
    }
    source += escapeRegExp(char);
    index += 1;
  }
  return new RegExp(`${source}$`, caseInsensitive ? "i" : "");
}

/** The literal directory a pattern starts with, before its first glob byte. */
export function globBase(pattern: string): string {
  const at = pattern.search(/[*?[]/);
  if (at < 0) return pattern;
  const prefix = pattern.slice(0, at);
  const slash = prefix.lastIndexOf("/");
  return slash <= 0 ? "/" : prefix.slice(0, slash);
}

export function hasGlob(pattern: string): boolean {
  return /[*?[]/.test(pattern);
}
