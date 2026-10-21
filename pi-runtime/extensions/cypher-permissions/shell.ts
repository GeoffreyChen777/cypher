/**
 * A conservative reading of a bash command line, for command rules: every
 * simple command the line can run, with its words and the files its
 * redirections open, or `opaque` when the line uses something whose effect
 * depends on more than that.
 *
 * Understood:
 * - words, quotes, escapes, comments, `~` and `~/` at the start of a word;
 * - `&&`, `||`, `;`, `|`, `|&` and newlines between commands;
 * - redirections: to and from files (their paths are reported, for the
 *   caller to check), copies like `2>&1`, heredocs and here-strings;
 * - `if`/`then`/`elif`/`else`/`fi`, `while`/`until`/`do`/`done`,
 *   `for NAME in …; do`, `!` and `[[ … ]]`: every command in them is read as
 *   one that may run;
 * - `$(…)` and backticks: the commands inside are read too;
 * - a `NAME=value` assignment standing alone, when `assignable(NAME)`;
 * - `NAME=value` assignments in front of a command, when
 *   `prefixAllowed(NAME)` for each: the command reports their names
 *   (`env`);
 * - `&` after a command: it runs in the background, apart from the line's
 *   shell (`scoped`).
 *
 * A word whose text is known only when it runs (a parameter, a
 * substitution, an unquoted glob or brace list) becomes [`UNKNOWN`]: it may
 * stand for any words, so in a pattern only `**` matches it.
 *
 * Everything else (subshells, groups, `case`, functions, arithmetic,
 * process substitution, a substitution inside `${…}` or an unquoted
 * heredoc, an assignment in front of a command that changes how programs
 * run) makes the line opaque: no rule can match it, so it runs in the
 * sandbox unless it is approved as written.
 */

/** A word known only when the line runs. */
export const UNKNOWN = "\0";

export interface SimpleCommand {
  /** Its words; a word known only when it runs is [`UNKNOWN`]. Empty for a bare redirection. */
  words: string[];
  /** Files its redirections read (`< file`), as written. */
  reads: string[];
  /** Files its redirections write (`> file`, `>> file`), as written. */
  writes: string[];
  /**
   * Whether it runs only sometimes, or apart from the line's shell: after
   * `&&` or `||`, in a pipeline, inside `if`, a loop or a substitution. A
   * `cd` here leaves the directory unknown.
   */
  scoped: boolean;
  /** Inside a loop: a `cd` here moves every command of the loop. */
  loop: boolean;
  /** Variables assigned in front of it (`NAME=value cmd`), by name. */
  env: string[];
}

export type ParsedLine = { kind: "simple"; commands: SimpleCommand[] } | { kind: "opaque"; reason: string };

export interface ParseOptions {
  /** Whether `NAME=value` alone on a command may stand (it changes nothing a program sees). */
  assignable?: (name: string) => boolean;
  /** Whether `NAME=value` in front of a command may stand (it does not change how programs load or run code). */
  prefixAllowed?: (name: string) => boolean;
}

/** Words that open or close a compound command bash has and this reader does not. */
const UNSUPPORTED = new Set(["case", "esac", "select", "function", "coproc", "{", "}", "]]"]);
/** Redirection targets that are no file. */
const NO_FILE = new Set(["/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr", "/dev/tty"]);

class Opaque extends Error {}

export function parseShell(line: string, home: string, options: ParseOptions = {}): ParsedLine {
  try {
    const commands: SimpleCommand[] = [];
    read(line, home, options, commands, false);
    return { kind: "simple", commands };
  } catch (error) {
    if (error instanceof Opaque) return { kind: "opaque", reason: error.message };
    throw error;
  }
}

type Redirect = "read" | "write" | "readwrite" | "copy" | "herestring" | "heredoc" | "heredoc-tabs";

/** Read `source` into `out`: the commands of a line, or of a substitution (`nested`). */
function read(source: string, home: string, options: ParseOptions, out: SimpleCommand[], nested: boolean): void {
  // The word being read.
  let word = "";
  let inWord = false;
  /** Its unquoted characters (globs, braces, assignments, fd numbers). */
  let raw = "";
  /** Some of it is quoted (a keyword never is; a heredoc's delimiter then is literal). */
  let quoted = false;
  /** Some of it is known only when it runs. */
  let unknown = false;

  // The command being read.
  let words: string[] = [];
  let reads: string[] = [];
  let writes: string[] = [];
  let assigns: string[] = [];
  /** Assignments in front of the command's program. */
  let env: string[] = [];
  let redirect: Redirect | undefined;
  /** The command after `&&`, `||` or `|`. */
  let chained = false;

  // Where in the line.
  /** Open `if` and loops, innermost last. */
  const blocks: ("if" | "loop")[] = [];
  /** `for`: 1 wants the name, 2 `in` (or the end), 3 reads the words. */
  let forHeader = 0;
  /** Inside `[[ … ]]`. */
  let test = false;
  /** Heredocs whose bodies start after this line. */
  const heredocs: { delimiter: string; tabs: boolean; literal: boolean }[] = [];

  const resetWord = () => {
    word = "";
    raw = "";
    inWord = false;
    quoted = false;
    unknown = false;
  };

  const endWord = () => {
    if (!inWord) return;
    const text = word;
    const unquoted = raw;
    const known = !unknown && !globbed(unquoted);
    const plain = !quoted && known && unquoted === text;
    resetWord();
    if (redirect) {
      const kind = redirect;
      redirect = undefined;
      if (kind === "herestring") return;
      if (kind === "heredoc" || kind === "heredoc-tabs") {
        if (!known) throw new Opaque("a computed heredoc delimiter");
        heredocs.push({ delimiter: text, tabs: kind === "heredoc-tabs", literal: !plain });
        return;
      }
      if (kind === "copy") {
        if (!/^(\d+|-)$/.test(text)) throw new Opaque("a redirection to a computed file");
        return;
      }
      if (!known) throw new Opaque("a redirection to a computed path");
      if (NO_FILE.has(text)) return;
      if (kind !== "write") reads.push(text);
      if (kind !== "read") writes.push(text);
      return;
    }
    if (forHeader === 3) return;
    if (test) {
      if (plain && text === "]]") test = false;
      return;
    }
    if (words.length === 0 && plain) {
      if (forHeader === 1) {
        if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(text)) throw new Opaque("`for`");
        forHeader = 2;
        return;
      }
      if (forHeader === 2) {
        if (text === "in") forHeader = 3;
        else if (text === "do") forHeader = 0;
        else throw new Opaque("`for`");
        return;
      }
      if (assigns.length === 0) {
        switch (text) {
          case "if":
            blocks.push("if");
            return;
          case "while":
          case "until":
            blocks.push("loop");
            return;
          case "for":
            blocks.push("loop");
            forHeader = 1;
            return;
          case "fi":
            if (blocks.pop() !== "if") throw new Opaque("an unmatched `fi`");
            return;
          case "done":
            if (blocks.pop() !== "loop") throw new Opaque("an unmatched `done`");
            return;
          case "then":
          case "elif":
          case "else":
          case "do":
          case "!":
            return;
          case "[[":
            test = true;
            return;
        }
        if (UNSUPPORTED.has(text)) throw new Opaque(`\`${text}\``);
      }
    }
    if (words.length === 0) {
      const assignment = /^([A-Za-z_][A-Za-z0-9_]*)\+?=/.exec(unquoted);
      if (assignment && text.startsWith(assignment[0])) {
        assigns.push(assignment[1]);
        return;
      }
    }
    if (assigns.length) {
      if (!assigns.every((name) => options.prefixAllowed?.(name))) throw new Opaque("a variable assignment in front of a command");
      env = assigns;
      assigns = [];
    }
    words.push(known ? text : UNKNOWN);
  };

  const endCommand = (next: "and-or" | "pipe" | "background" | "end") => {
    endWord();
    if (redirect) throw new Opaque("a redirection without a target");
    if (test) throw new Opaque("an unfinished `[[`");
    if (forHeader === 1) throw new Opaque("`for`");
    if (forHeader > 1) forHeader = 0;
    for (const name of assigns) {
      if (!options.assignable?.(name)) throw new Opaque("a variable assignment");
    }
    if (words.length || reads.length || writes.length) {
      const loop = blocks.includes("loop");
      out.push({ words, reads, writes, loop, env, scoped: nested || chained || next === "pipe" || next === "background" || blocks.length > 0 });
    }
    words = [];
    reads = [];
    writes = [];
    assigns = [];
    env = [];
    chained = next === "and-or" || next === "pipe";
  };

  /** Skip `$…` at `i` (in or out of double quotes); returns the index of its last character. */
  const dollar = (i: number, inQuotes: boolean): number => {
    const next = source[i + 1];
    if (next === "(") {
      if (source[i + 2] === "(") throw new Opaque("arithmetic");
      const close = closingParen(source, i + 2);
      read(source.slice(i + 2, close), home, options, out, true);
      unknown = true;
      inWord = true;
      return close;
    }
    if (next === "{") {
      const close = source.indexOf("}", i + 2);
      if (close < 0) throw new Opaque("an unterminated `${`");
      if (/[$`(]/.test(source.slice(i + 2, close))) throw new Opaque("a substitution inside `${…}`");
      unknown = true;
      inWord = true;
      return close;
    }
    if (next !== undefined && /[A-Za-z_]/.test(next)) {
      let j = i + 1;
      while (j + 1 < source.length && /[A-Za-z0-9_]/.test(source[j + 1])) j++;
      unknown = true;
      inWord = true;
      return j;
    }
    if (next !== undefined && /[0-9@*#?$!-]/.test(next)) {
      unknown = true;
      inWord = true;
      return i + 1;
    }
    if (!inQuotes && (next === "'" || next === '"')) throw new Opaque("`$'…'` quoting");
    // A lone `$` is itself.
    word += "$";
    if (!inQuotes) raw += "$";
    inWord = true;
    return i;
  };

  /** Read the backtick substitution at `i`; returns the index of its closing backtick. */
  const backtick = (i: number): number => {
    let inner = "";
    let j = i + 1;
    for (; j < source.length && source[j] !== "`"; j++) {
      if (source[j] === "\\" && /[`$\\]/.test(source[j + 1] ?? "")) j++;
      inner += source[j];
    }
    if (j >= source.length) throw new Opaque("an unterminated backtick");
    read(inner, home, options, out, true);
    unknown = true;
    inWord = true;
    return j;
  };

  /** The bodies of this line's heredocs, from `i` (a newline); returns the index of the last body's last character. */
  const heredocBodies = (i: number): number => {
    let at = i + 1;
    for (const heredoc of heredocs) {
      for (;;) {
        if (at > source.length) throw new Opaque("an unterminated heredoc");
        const end = source.indexOf("\n", at);
        const text = source.slice(at, end < 0 ? source.length : end);
        at = end < 0 ? source.length + 1 : end + 1;
        if ((heredoc.tabs ? text.replace(/^\t+/, "") : text) === heredoc.delimiter) break;
        if (!heredoc.literal && /`|\$\(/.test(text)) throw new Opaque("a substitution in a heredoc");
      }
    }
    heredocs.length = 0;
    // The newline after the last delimiter, read as the end of a command.
    return at - 2;
  };

  for (let i = 0; i < source.length; i++) {
    const char = source[i];
    const next = source[i + 1];
    if (test && "&|()<>".includes(char)) {
      // Operators inside `[[ … ]]` compare; nothing runs.
      word += char;
      raw += char;
      inWord = true;
      continue;
    }
    switch (char) {
      case " ":
      case "\t":
        endWord();
        break;
      case "\n":
        endCommand("end");
        if (heredocs.length) i = heredocBodies(i);
        break;
      case ";":
        if (next === ";" || next === "&") throw new Opaque("`case`");
        endCommand("end");
        break;
      case "&":
        if (next === "&") {
          endCommand("and-or");
          i++;
        } else if (next === ">") {
          endWord();
          redirect = "write";
          i += source[i + 2] === ">" ? 2 : 1;
          if (source[i + 1] === "(") throw new Opaque("process substitution");
        } else {
          // In the background: it runs apart from the line's shell, and
          // what follows runs whether it does or not.
          if (inWord || words.length || reads.length || writes.length || redirect) endCommand("background");
          else throw new Opaque("a background job");
        }
        break;
      case "|":
        if (next === "|") {
          endCommand("and-or");
          i++;
        } else {
          endCommand("pipe");
          if (next === "&") i++;
        }
        break;
      case "<":
      case ">": {
        // A number right before it is the file descriptor it redirects.
        if (inWord && !quoted && !unknown && raw === word && /^\d+$/.test(word)) resetWord();
        else endWord();
        if (redirect) throw new Opaque("a redirection without a target");
        if (char === ">") {
          if (next === "&") {
            redirect = "copy";
            i++;
          } else {
            redirect = "write";
            if (next === ">" || next === "|") i++;
          }
        } else if (next === "<") {
          if (source[i + 2] === "<") {
            redirect = "herestring";
            i += 2;
          } else if (source[i + 2] === "-") {
            redirect = "heredoc-tabs";
            i += 2;
          } else {
            redirect = "heredoc";
            i++;
          }
        } else if (next === "&") {
          redirect = "copy";
          i++;
        } else if (next === ">") {
          redirect = "readwrite";
          i++;
        } else {
          redirect = "read";
        }
        if (source[i + 1] === "(") throw new Opaque("process substitution");
        break;
      }
      case "(":
      case ")":
        throw new Opaque("a subshell");
      case "`":
        i = backtick(i);
        break;
      case "$":
        i = dollar(i, false);
        break;
      case "#":
        if (!inWord) {
          while (i + 1 < source.length && source[i + 1] !== "\n") i++;
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
        quoted = true;
        i++;
        break;
      case "'": {
        const end = source.indexOf("'", i + 1);
        if (end < 0) throw new Opaque("an unterminated quote");
        word += source.slice(i + 1, end);
        inWord = true;
        quoted = true;
        i = end;
        break;
      }
      case '"': {
        inWord = true;
        quoted = true;
        let j = i + 1;
        for (; j < source.length && source[j] !== '"'; j++) {
          const inner = source[j];
          if (inner === "\\") {
            const escaped = source[j + 1];
            if (escaped === "\n") {
              j++;
            } else if (escaped !== undefined && '$`"\\'.includes(escaped)) {
              word += escaped;
              j++;
            } else {
              word += inner;
            }
          } else if (inner === "$") {
            j = dollar(j, true);
          } else if (inner === "`") {
            j = backtick(j);
          } else {
            word += inner;
          }
        }
        if (j >= source.length) throw new Opaque("an unterminated quote");
        i = j;
        break;
      }
      case "~":
        if (!inWord) {
          inWord = true;
          if (next === undefined || " \t\n;|&/<>)".includes(next)) {
            word += home;
          } else {
            // `~user`, `~+`: known only when it runs.
            unknown = true;
            while (i + 1 < source.length && !" \t\n;|&/<>()".includes(source[i + 1])) i++;
          }
          break;
        }
        word += char;
        raw += char;
        break;
      default:
        word += char;
        raw += char;
        inWord = true;
    }
  }
  endCommand("end");
  if (heredocs.length) throw new Opaque("an unterminated heredoc");
  if (blocks.length) throw new Opaque(`an unfinished \`${blocks.at(-1) === "if" ? "if" : "loop"}\``);
}

/** Whether a word's unquoted part expands to file names or several words. */
function globbed(raw: string): boolean {
  if (raw === "[" || raw === "]" || raw === "[[" || raw === "]]") return false;
  return /[*?[]/.test(raw) || /\{[^{}]*(,|\.\.)[^{}]*\}/.test(raw);
}

/** The index of the `)` closing a `$(` whose contents start at `start`. */
function closingParen(source: string, start: number): number {
  let depth = 1;
  let wordStart = true;
  for (let i = start; i < source.length; i++) {
    const char = source[i];
    switch (char) {
      case "\\":
        i++;
        wordStart = false;
        continue;
      case "'": {
        const end = source.indexOf("'", i + 1);
        if (end < 0) throw new Opaque("an unterminated quote");
        i = end;
        break;
      }
      case '"':
        i = closingQuote(source, i);
        break;
      case "`": {
        let j = i + 1;
        for (; j < source.length && source[j] !== "`"; j++) if (source[j] === "\\") j++;
        if (j >= source.length) throw new Opaque("an unterminated backtick");
        i = j;
        break;
      }
      case "#":
        if (wordStart) {
          while (i + 1 < source.length && source[i + 1] !== "\n") i++;
        }
        break;
      case "<":
        // A heredoc's body could hold the `)`.
        if (source[i + 1] === "<" && source[i + 2] !== "<") throw new Opaque("a heredoc in a substitution");
        break;
      case "(":
        depth++;
        break;
      case ")":
        if (--depth === 0) return i;
        break;
    }
    wordStart = " \t\n;|&(".includes(char);
  }
  throw new Opaque("an unterminated substitution");
}

/** The index of the `"` closing the double quote at `start`. */
function closingQuote(source: string, start: number): number {
  for (let i = start + 1; i < source.length; i++) {
    const char = source[i];
    if (char === "\\") i++;
    else if (char === '"') return i;
    else if (char === "$" && source[i + 1] === "(") i = closingParen(source, i + 2);
    else if (char === "`") {
      let j = i + 1;
      for (; j < source.length && source[j] !== "`"; j++) if (source[j] === "\\") j++;
      i = j;
    }
  }
  throw new Opaque("an unterminated quote");
}
