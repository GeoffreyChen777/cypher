/**
 * A conservative reading of a bash command line, for command rules: the
 * simple commands it runs and their words, or `opaque` when the line uses
 * anything whose effect depends on more than its text.
 *
 * Understood: words, single quotes, double quotes without `$`, backticks or
 * escapes other than `\"` and `\\`, a backslash escaping one character, `~`
 * and `~/` at the start of a word, comments, and the operators `&&`, `||`,
 * `;`, `|` and newlines between commands. `2>&1` and redirection to
 * `/dev/null` are dropped. Everything else (expansions, substitutions, other
 * redirections, unquoted globs and braces, subshells, groups, background
 * jobs, assignments, compound commands) makes the line opaque: no rule can
 * match it, so it runs in the sandbox unless it is approved as written.
 */

export type ParsedLine = { kind: "simple"; commands: string[][] } | { kind: "opaque"; reason: string };

const KEYWORDS = new Set(["if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "function", "select", "time", "coproc", "[[", "]]", "!", "{", "}"]);
/** Redirections that write nothing anywhere. */
const HARMLESS_REDIRECTS = new Set(["2>&1", ">&2", "1>&2", ">/dev/null", "1>/dev/null", "2>/dev/null", "&>/dev/null"]);

class Opaque extends Error {}

export function parseShell(line: string, home: string): ParsedLine {
  try {
    return { kind: "simple", commands: split(line, home) };
  } catch (error) {
    if (error instanceof Opaque) return { kind: "opaque", reason: error.message };
    throw error;
  }
}

function split(line: string, home: string): string[][] {
  const commands: string[][] = [];
  let words: string[] = [];
  let word = "";
  /** A word is being built (an empty quoted string is still a word). */
  let inWord = false;
  /** The word has an unquoted part (for `~`, globs and redirections). */
  let raw = "";

  const endWord = () => {
    if (!inWord) return;
    if (HARMLESS_REDIRECTS.has(raw) && raw === word) {
      // Dropped: it changes no file and runs nothing.
    } else {
      if (/[<>]/.test(raw)) throw new Opaque("redirection");
      if (/[*?[]/.test(raw)) throw new Opaque("an unquoted glob");
      if (/[{}]/.test(raw)) throw new Opaque("braces");
      if (words.length === 0 && /^[A-Za-z_][A-Za-z0-9_]*\+?=/.test(raw)) throw new Opaque("a variable assignment");
      words.push(word);
    }
    word = "";
    raw = "";
    inWord = false;
  };
  const endCommand = () => {
    endWord();
    if (words.length) {
      if (KEYWORDS.has(words[0])) throw new Opaque(`\`${words[0]}\``);
      commands.push(words);
    }
    words = [];
  };

  for (let i = 0; i < line.length; i++) {
    const char = line[i];
    const next = line[i + 1];
    switch (char) {
      case " ":
      case "\t":
        endWord();
        break;
      case "\n":
      case ";":
        if (char === ";" && next === ";") throw new Opaque("`;;`");
        endCommand();
        break;
      case "&":
        if (next === "&") {
          endCommand();
          i++;
        } else if (raw.endsWith(">") || raw === "" && next === ">") {
          // `2>&1`, `&>/dev/null`: part of a redirection word.
          word += char;
          raw += char;
          inWord = true;
        } else {
          throw new Opaque("a background job");
        }
        break;
      case "|":
        if (next === "&") throw new Opaque("`|&`");
        endCommand();
        if (next === "|") i++;
        break;
      case "(":
      case ")":
        throw new Opaque("a subshell");
      case "`":
        throw new Opaque("command substitution");
      case "$":
        throw new Opaque("an expansion");
      case "#":
        if (!inWord) {
          while (i + 1 < line.length && line[i + 1] !== "\n") i++;
          break;
        }
        word += char;
        raw += char;
        break;
      case "\\":
        if (next === undefined) throw new Opaque("a trailing backslash");
        if (next === "\n") {
          i++;
          break;
        }
        word += next;
        inWord = true;
        i++;
        break;
      case "'": {
        const end = line.indexOf("'", i + 1);
        if (end < 0) throw new Opaque("an unterminated quote");
        word += line.slice(i + 1, end);
        inWord = true;
        i = end;
        break;
      }
      case '"': {
        let j = i + 1;
        for (; j < line.length && line[j] !== '"'; j++) {
          const inner = line[j];
          if (inner === "$" || inner === "`") throw new Opaque("an expansion in double quotes");
          if (inner === "\\") {
            const escaped = line[j + 1];
            if (escaped !== '"' && escaped !== "\\") throw new Opaque("an escape in double quotes");
            word += escaped;
            j++;
            continue;
          }
          word += inner;
        }
        if (j >= line.length) throw new Opaque("an unterminated quote");
        inWord = true;
        i = j;
        break;
      }
      case "~":
        if (!inWord && (next === undefined || next === "/" || next === " " || next === "\t" || next === "\n" || next === ";" || next === "|" || next === "&")) {
          word += home;
          inWord = true;
          break;
        }
        if (!inWord) throw new Opaque("`~user`");
        word += char;
        raw += char;
        break;
      default:
        word += char;
        raw += char;
        inWord = true;
    }
  }
  endCommand();
  return commands;
}
