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
 *   caller to check; a path known only when it runs as [`UNKNOWN`]), copies
 *   like `2>&1`, heredocs and here-strings;
 * - `if`/`then`/`elif`/`else`/`fi`, `while`/`until`/`do`/`done`,
 *   `for NAME in …; do`, `case WORD in PATTERN) … ;; esac`, `{ …; }`, `!`
 *   and `[[ … ]]`: every command in them is read as one that may run;
 * - `$(…)` and backticks: the commands inside are read too, a heredoc in
 *   them as well (`git commit -m "$(cat <<'EOF' … EOF)"`);
 * - `$((…))`: a number known only when the line runs, when nothing in it
 *   runs a command;
 * - a `NAME=value` assignment standing alone, when `assignable(NAME)`;
 * - `NAME=value` assignments in front of a command, when
 *   `prefixAllowed(NAME)` for each: the command reports their names
 *   (`env`);
 * - `&` after a command: it runs in the background, apart from the line's
 *   shell (`scoped`);
 * - `( … )` at the start of a command: a subshell. Its commands are read
 *   like the line's (`subshell` tells them apart); a `cd` or an assignment in
 *   it stays in it;
 * - `$NAME` and `${NAME}` after `NAME=value` (or `export NAME=value`) set
 *   to a literal earlier in the line, where it surely ran (not after `&&` or
 *   `||`, in a pipeline, `if`, loop or substitution) and nothing since could
 *   have changed it (`read`, `declare`, `eval`, `source`, a program known
 *   only when it runs, …; `unset` forgets only what it names): the value,
 *   when expanding it cannot split or glob. `$HOME` is the home folder, and
 *   `~` starting a value expands to it, as bash does. `$PWD` is the
 *   directory the line starts in (`cwd`), in a line that runs nothing that
 *   could move its shell or set it (`cd`, `eval`, `source`, `export`, …);
 *
 * The line also reports the variables its own shell expands (`expands`):
 * a secret's name there is a secret this line hands to its commands, while
 * `$NAME` in single quotes is text for whatever reads it (`ssh host 'echo
 * $NAME'` is the host's variable).
 *
 * A word whose text is known only when it runs (a parameter, a
 * substitution, an unquoted glob or brace list) becomes [`UNKNOWN`]: it may
 * stand for any words, so in a pattern only `**` matches it.
 *
 * Everything else (nested subshells, functions, `(( … ))`, `select`,
 * process substitution, a substitution inside `${…}`, `$((…))` or an
 * unquoted heredoc, an assignment in front of a command that changes how
 * programs run) makes the line opaque: no rule can match it, so it runs in the
 * sandbox unless it is approved as written.
 */

/** A word known only when the line runs. */
export const UNKNOWN = "\0";

export interface SimpleCommand {
  /** Its words; a word known only when it runs is [`UNKNOWN`]. Empty for a bare redirection. */
  words: string[];
  /** Files its redirections read (`< file`), as written; [`UNKNOWN`] for a path known only when it runs. */
  reads: string[];
  /** Files its redirections write (`> file`, `>> file`), as written; [`UNKNOWN`] for a path known only when it runs. */
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
  /** Their values, in order; [`UNKNOWN`] for one known only when it runs. */
  envValues?: string[];
  /**
   * For each word known only when it runs, the text it surely starts with
   * (`~/.ssh/id_` for `~/.ssh/id_*`, `user.name=` for `user.name="$(…)"`),
   * else undefined; one per word. Absent when every word is known. It says
   * where an unknown word points, never what a rule may match.
   */
  hints?: (string | undefined)[];
  /** In a subshell `( … )`: which one, numbered from 1 in the line. */
  subshell?: number;
}

export type ParsedLine =
  | {
      kind: "simple";
      commands: SimpleCommand[];
      /** Variables the line's own shell expands (`$NAME`, `${NAME}`, in an unquoted heredoc too), by name. */
      expands: string[];
    }
  | { kind: "opaque"; reason: string };

export interface ParseOptions {
  /** Whether `NAME=value` alone on a command may stand (it changes nothing a program sees). */
  assignable?: (name: string) => boolean;
  /** Whether `NAME=value` in front of a command may stand (it does not change how programs load or run code). */
  prefixAllowed?: (name: string) => boolean;
  /** Whether `export NAME=value` may keep its value known (not a variable bash itself reads); without it, `export` forgets every value. */
  exportable?: (name: string) => boolean;
  /** The directory the line starts in: what `$PWD` is, in a line that never changes directory. */
  cwd?: string;
}

/** Words that open or close a compound command bash has and this reader does not. */
const UNSUPPORTED = new Set(["select", "function", "coproc", "]]"]);
/** Redirection targets that are no file. */
const NO_FILE = new Set(["/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr", "/dev/tty"]);

class Opaque extends Error {}

/** Variables the line set to a literal (undefined: set to something not known), and whether any could have changed since. */
interface Variables {
  values: Map<string, string | undefined>;
  poisoned: boolean;
  /** Every variable the line's shell expands, by name (shared by its subshells and substitutions). */
  expanded: Set<string>;
}

/**
 * Builtins that move the line's shell or run code in it (on top of
 * [`SETS_VARIABLES`], which could set `PWD` itself): in a line that runs
 * one, anywhere, `$PWD` is known only when it runs.
 */
const MOVES_SHELL: ReadonlySet<string> = new Set(["cd", "pushd", "popd", "exec", "trap", "set", "shopt", "enable", "alias", "hash"]);

/**
 * Builtins that set variables in ways the reader does not follow. (`set`
 * changes options and the positional parameters, which are never known, but
 * no named variable.)
 */
const SETS_VARIABLES: ReadonlySet<string> = new Set([
  "read", "mapfile", "readarray", "export", "declare", "typeset", "local", "readonly", "let", "getopts", "unset", "eval", "source", ".", "builtin", "command", "wait",
]);

export function parseShell(line: string, home: string, options: ParseOptions = {}): ParsedLine {
  if (options.cwd === undefined) return readLine(line, home, options, undefined);
  // `$PWD` is where the line starts, as long as nothing in it can move its
  // shell or set the variable. The line is read with it known, and its
  // commands (as bash reads them, so a quoted `'cd'` counts) say whether
  // that holds; if not, it is read again with `$PWD` known only when it runs.
  const parsed = readLine(line, home, options, options.cwd);
  if (parsed.kind !== "simple" || !parsed.expands.includes("PWD")) return parsed;
  const moved = parsed.commands.some((command) => {
    const program = command.words[0];
    if (program === undefined) return false;
    if (program === UNKNOWN || MOVES_SHELL.has(program) || SETS_VARIABLES.has(program)) return true;
    return program === "printf" && command.words.some((word) => word === UNKNOWN || word.startsWith("-v"));
  });
  return moved ? readLine(line, home, options, undefined) : parsed;
}

function readLine(line: string, home: string, options: ParseOptions, cwd: string | undefined): ParsedLine {
  try {
    const commands: SimpleCommand[] = [];
    // `$HOME` is the home folder `~` stands for, until the line changes it.
    const values = new Map<string, string | undefined>([["HOME", home]]);
    if (cwd !== undefined) values.set("PWD", cwd);
    const vars: Variables = { values, poisoned: false, expanded: new Set() };
    read(line, home, options, commands, false, vars, { next: 1 });
    return { kind: "simple", commands, expands: [...vars.expanded] };
  } catch (error) {
    if (error instanceof Opaque) return { kind: "opaque", reason: error.message };
    throw error;
  }
}

type Redirect = "read" | "write" | "readwrite" | "copy" | "herestring" | "heredoc" | "heredoc-tabs";

/**
 * Read `source` into `out`: the commands of a line, or of a substitution
 * (`nested`), or of a subshell (`subshell`, its number).
 */
function read(
  source: string,
  home: string,
  options: ParseOptions,
  out: SimpleCommand[],
  nested: boolean,
  vars: Variables,
  subshells: { next: number },
  subshell?: number,
): void {
  // The word being read.
  let word = "";
  let inWord = false;
  /** Its unquoted characters (globs, braces, assignments, fd numbers). */
  let raw = "";
  /** Some of it is quoted (a keyword never is; a heredoc's delimiter then is literal). */
  let quoted = false;
  /** Some of it is known only when it runs. */
  let unknown = false;
  /** How much of it was read when it first became unknown. */
  let knownUpTo = 0;
  /** A part of the word is known only when it runs, from here on. */
  const becomesUnknown = () => {
    if (!unknown) knownUpTo = word.length;
    unknown = true;
  };

  // The command being read.
  let words: string[] = [];
  let hints: (string | undefined)[] = [];
  let reads: string[] = [];
  let writes: string[] = [];
  let assigns: string[] = [];
  /** Each assignment's value, when it is a literal. */
  let assignValues: (string | undefined)[] = [];
  /** Assignments in front of the command's program. */
  let env: string[] = [];
  let envValues: string[] = [];
  let redirect: Redirect | undefined;
  /** The command after `&&`, `||` or `|`. */
  let chained = false;
  /** A subshell just closed: only operators and redirections may follow it. */
  let afterSubshell = false;

  // Where in the line.
  /** Open `if`, loops, `case` and `{ … }` groups, innermost last. */
  const blocks: ("if" | "loop" | "case" | "group")[] = [];
  /**
   * Where each open `case` is, innermost last: after `case` comes its word
   * (`subject`), then `in`, then for each clause its patterns up to `)`
   * and its commands (`body`) up to `;;`.
   */
  const cases: { phase: "subject" | "in" | "pattern" | "body"; started: boolean }[] = [];
  /** The `case` whose syntax applies here: the innermost block, when it is one. */
  const openCase = () => (blocks.at(-1) === "case" ? cases.at(-1) : undefined);
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
    knownUpTo = 0;
  };

  const endWord = () => {
    if (!inWord) return;
    const text = word;
    const unquoted = raw;
    const known = !unknown && !globbed(unquoted);
    const plain = !quoted && known && unquoted === text;
    // What an unknown word surely starts with: up to its first substitution, or its first glob character.
    const glob = text.search(/[*?[{]/);
    const hint = known ? undefined : text.slice(0, Math.min(unknown ? knownUpTo : text.length, glob < 0 ? text.length : glob));
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
      // A path known only when it runs is still a file the command opens: which one, nobody can say yet.
      const path = known ? text : UNKNOWN;
      if (known && NO_FILE.has(text)) return;
      if (kind !== "write") reads.push(path);
      if (kind !== "read") writes.push(path);
      return;
    }
    if (afterSubshell) throw new Opaque("a subshell");
    const clause = openCase();
    if (clause && clause.phase !== "body") {
      if (clause.phase === "subject") clause.phase = "in";
      else if (clause.phase === "in") {
        if (!plain || text !== "in") throw new Opaque("`case`");
        clause.phase = "pattern";
        clause.started = false;
      } else if (!clause.started && plain && text === "esac") {
        blocks.pop();
        cases.pop();
      } else clause.started = true;
      // A pattern names no command and no file.
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
        vars.values.set(text, undefined);
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
          case "case":
            blocks.push("case");
            cases.push({ phase: "subject", started: false });
            return;
          case "esac":
            if (blocks.pop() !== "case") throw new Opaque("an unmatched `esac`");
            cases.pop();
            return;
          case "{":
            blocks.push("group");
            return;
          case "}":
            if (blocks.pop() !== "group") throw new Opaque("an unmatched `}`");
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
        assignValues.push(known && !assignment[0].endsWith("+=") ? assignedValue(text.slice(assignment[0].length), unquoted.slice(assignment[0].length), home) : undefined);
        return;
      }
    }
    if (assigns.length) {
      if (!assigns.every((name) => options.prefixAllowed?.(name))) throw new Opaque("a variable assignment in front of a command");
      env = assigns;
      envValues = assignValues.map((value) => value ?? UNKNOWN);
      assigns = [];
      assignValues = [];
    }
    words.push(known ? text : UNKNOWN);
    hints.push(hint);
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
    const scoped = nested || chained || next === "pipe" || next === "background" || blocks.length > 0;
    // A value is known after an assignment that surely ran in the line's shell.
    if (words.length === 0) assigns.forEach((name, index) => vars.values.set(name, scoped ? undefined : assignValues[index]));
    const program = words[0];
    if (program === "export" && exported(words, options)) {
      for (const word of words.slice(1)) {
        const [name, ...value] = word.split("=");
        // `~` in an exported value: not followed here.
        vars.values.set(name, scoped || value.join("=").includes("~") ? undefined : value.join("="));
      }
    } else if (program === "unset" && words.slice(1).every((word) => word === "-v" || /^[A-Za-z_][A-Za-z0-9_]*$/.test(word))) {
      // It empties the variables it names, and only those.
      for (const name of words.slice(1)) if (name !== "-v") vars.values.set(name, undefined);
    } else if (program !== undefined && (program === UNKNOWN || SETS_VARIABLES.has(program) || (program === "printf" && words.some((word) => word === UNKNOWN || word.startsWith("-v"))))) {
      vars.poisoned = true;
    }
    if (words.length || reads.length || writes.length) {
      const loop = blocks.includes("loop");
      out.push({
        words,
        reads,
        writes,
        loop,
        env,
        scoped,
        ...(envValues.length ? { envValues } : {}),
        ...(hints.some((hint) => hint !== undefined) ? { hints } : {}),
        ...(subshell !== undefined ? { subshell } : {}),
      });
    }
    afterSubshell = false;
    words = [];
    hints = [];
    reads = [];
    writes = [];
    assigns = [];
    assignValues = [];
    env = [];
    envValues = [];
    chained = next === "and-or" || next === "pipe";
  };

  /**
   * A variable's value, when the line set it to a literal and expanding it
   * here gives exactly that word: in double quotes always, unquoted when it
   * cannot split, glob or vanish. Never in a heredoc's delimiter, which is
   * not expanded.
   */
  const knownValue = (name: string, inQuotes: boolean): string | undefined => {
    if (vars.poisoned || redirect === "heredoc" || redirect === "heredoc-tabs") return undefined;
    const value = vars.values.get(name);
    if (value === undefined) return undefined;
    if (!inQuotes && (value === "" || /[\s*?[\]]/.test(value))) return undefined;
    return value;
  };

  /** A known variable's value joins the word; it is not a keyword, an assignment or a glob there. */
  const expand = (value: string) => {
    word += value;
    quoted = true;
    inWord = true;
  };

  /** Skip `$…` at `i` (in or out of double quotes); returns the index of its last character. */
  const dollar = (i: number, inQuotes: boolean): number => {
    const next = source[i + 1];
    if (next === "(") {
      if (source[i + 2] === "(") {
        // `$((…))`: a number. Nothing in it may run a command; an assignment in it changes a variable unseen.
        let inner = i + 3;
        for (let depth = 1; inner < source.length; inner++) {
          if (source[inner] === "(") depth++;
          else if (source[inner] === ")" && --depth === 0) break;
        }
        if (source[inner + 1] !== ")") throw new Opaque("arithmetic");
        const expression = source.slice(i + 3, inner);
        if (/[`'"\\]|\$\(/.test(expression)) throw new Opaque("arithmetic");
        if (/(^|[^=!<>])=(?!=)|\+\+|--/.test(expression)) vars.poisoned = true;
        becomesUnknown();
        inWord = true;
        return inner + 1;
      }
      const close = closingParen(source, i + 2);
      read(source.slice(i + 2, close), home, options, out, true, vars, subshells, subshell);
      becomesUnknown();
      inWord = true;
      return close;
    }
    if (next === "{") {
      const close = source.indexOf("}", i + 2);
      if (close < 0) throw new Opaque("an unterminated `${`");
      if (/[$`(]/.test(source.slice(i + 2, close))) throw new Opaque("a substitution inside `${…}`");
      const name = source.slice(i + 2, close);
      const expanded = /^[#!]?([A-Za-z_][A-Za-z0-9_]*)/.exec(name)?.[1];
      if (expanded !== undefined) vars.expanded.add(expanded);
      const value = /^[A-Za-z_][A-Za-z0-9_]*$/.test(name) ? knownValue(name, inQuotes) : undefined;
      if (value !== undefined) expand(value);
      else becomesUnknown();
      inWord = true;
      return close;
    }
    if (next !== undefined && /[A-Za-z_]/.test(next)) {
      let j = i + 1;
      while (j + 1 < source.length && /[A-Za-z0-9_]/.test(source[j + 1])) j++;
      vars.expanded.add(source.slice(i + 1, j + 1));
      const value = knownValue(source.slice(i + 1, j + 1), inQuotes);
      if (value !== undefined) expand(value);
      else becomesUnknown();
      inWord = true;
      return j;
    }
    if (next !== undefined && /[0-9@*#?$!-]/.test(next)) {
      becomesUnknown();
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
    read(inner, home, options, out, true, vars, subshells, subshell);
    becomesUnknown();
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
        if (heredoc.literal) continue;
        if (/`|\$\(/.test(text)) throw new Opaque("a substitution in a heredoc");
        // The line's shell expands an unquoted heredoc's variables before the command reads it.
        for (const match of text.matchAll(/(?<!\\)\$\{?[#!]?([A-Za-z_][A-Za-z0-9_]*)/g)) vars.expanded.add(match[1]);
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
        if (next === ";" || next === "&") {
          // `;;`, `;&` and `;;&` end a `case` clause: its patterns come next.
          endCommand("end");
          const clause = openCase();
          if (clause?.phase !== "body") throw new Opaque("`case`");
          clause.phase = "pattern";
          clause.started = false;
          i += next === ";" && source[i + 2] === "&" ? 2 : 1;
          break;
        }
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
          if (inWord || words.length || reads.length || writes.length || redirect || afterSubshell) endCommand("background");
          else throw new Opaque("a background job");
        }
        break;
      case "|":
        if (openCase()?.phase === "pattern") {
          // Between a clause's patterns.
          endWord();
          break;
        }
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
      case "(": {
        const clause = openCase();
        if (clause?.phase === "pattern" && !inWord && !clause.started) {
          // `(pattern)`: the parenthesis before a clause's patterns says nothing.
          clause.started = true;
          break;
        }
        // A subshell starts a command; anywhere else `(` is something this reader does not follow (a function).
        if (inWord || words.length || assigns.length || redirect || test || forHeader || afterSubshell) throw new Opaque("a subshell");
        if (subshell !== undefined || nested) throw new Opaque("a nested subshell");
        if (next === "(") throw new Opaque("arithmetic");
        const close = closingParen(source, i + 1);
        // Its own shell: its assignments and `cd` stay in it, and what it changes no later word sees.
        const inner: Variables = { values: new Map(vars.values), poisoned: vars.poisoned, expanded: vars.expanded };
        const commands: SimpleCommand[] = [];
        read(source.slice(i + 1, close), home, options, commands, false, inner, subshells, subshells.next++);
        // Run only sometimes, or apart: then so is each command in it.
        const apart = chained || blocks.length > 0;
        const loop = blocks.includes("loop");
        for (const command of commands) out.push({ ...command, scoped: command.scoped || apart, loop: command.loop || loop });
        afterSubshell = true;
        i = close;
        break;
      }
      case ")": {
        const clause = openCase();
        if (clause?.phase !== "pattern") throw new Opaque("a subshell");
        // The end of a clause's patterns: its commands follow.
        endWord();
        clause.phase = "body";
        break;
      }
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
            becomesUnknown();
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
  if (blocks.length) throw new Opaque(`an unfinished \`${{ if: "if", loop: "loop", case: "case", group: "{" }[blocks.at(-1)!]}\``);
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
  /** Heredocs opened on the line being read: their bodies start after its newline. */
  const heredocs: { delimiter: string; tabs: boolean }[] = [];
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
        if (source[i + 1] !== "<") break;
        if (source[i + 2] === "<") {
          // A here-string: a word follows.
          i += 2;
          break;
        }
        {
          // A heredoc: its body could hold the `)`, so it is skipped as bash reads it.
          const heredoc = heredocDelimiter(source, i + 2);
          heredocs.push(heredoc);
          i = heredoc.end;
        }
        break;
      case "\n":
        if (heredocs.length) {
          i = heredocEnd(source, i, heredocs);
          heredocs.length = 0;
        }
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

/**
 * The delimiter of the heredoc whose `<<` ends just before `at`, read as
 * bash does (quotes removed), and the index of its last character. A
 * delimiter bash would compute is not followed.
 */
function heredocDelimiter(source: string, at: number): { delimiter: string; tabs: boolean; end: number } {
  let j = at;
  const tabs = source[j] === "-";
  if (tabs) j++;
  while (source[j] === " " || source[j] === "\t") j++;
  let delimiter = "";
  let any = false;
  for (; j < source.length; j++) {
    const char = source[j];
    if (char === "'" || char === '"') {
      const end = source.indexOf(char, j + 1);
      if (end < 0) throw new Opaque("an unterminated quote");
      const text = source.slice(j + 1, end);
      if (char === '"' && /[$`\\]/.test(text)) throw new Opaque("a computed heredoc delimiter");
      delimiter += text;
      j = end;
    } else if (char === "\\") {
      if (source[j + 1] === undefined) throw new Opaque("a trailing backslash");
      delimiter += source[++j];
    } else if (char === "$" || char === "`") throw new Opaque("a computed heredoc delimiter");
    else if (" \t\n;|&()<>".includes(char)) break;
    else delimiter += char;
    any = true;
  }
  if (!any) throw new Opaque("a heredoc without a delimiter");
  return { delimiter, tabs, end: j - 1 };
}

/** Past the bodies of the heredocs opened on the line that ends at `newline`: the index of the last body's last character. */
function heredocEnd(source: string, newline: number, heredocs: readonly { delimiter: string; tabs: boolean }[]): number {
  let at = newline + 1;
  for (const heredoc of heredocs) {
    for (;;) {
      if (at > source.length) throw new Opaque("an unterminated heredoc");
      const end = source.indexOf("\n", at);
      const text = source.slice(at, end < 0 ? source.length : end);
      at = end < 0 ? source.length + 1 : end + 1;
      if ((heredoc.tabs ? text.replace(/^\t+/, "") : text) === heredoc.delimiter) break;
    }
  }
  return at - 2;
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

/**
 * The value an assignment gives, known as written: `~` starting it (unquoted,
 * alone or before `/`) is the home folder, as bash expands it; any other `~`
 * (after `:`, `~user`) is not followed.
 */
function assignedValue(value: string, unquoted: string, home: string): string | undefined {
  if (!unquoted.includes("~")) return value;
  if ((unquoted === "~" || unquoted.startsWith("~/")) && value.startsWith(unquoted[0]) && !unquoted.slice(1).includes("~")) return home + value.slice(1);
  return undefined;
}

/** `export NAME=value …` with values known, for variables the line may keep (`exportable`). */
function exported(words: readonly string[], options: ParseOptions): boolean {
  if (!options.exportable || words.length < 2) return false;
  return words.slice(1).every((word) => {
    if (word === UNKNOWN) return false;
    const name = /^([A-Za-z_][A-Za-z0-9_]*)=/.exec(word)?.[1];
    return name !== undefined && options.exportable!(name);
  });
}
