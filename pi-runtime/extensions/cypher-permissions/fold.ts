/**
 * Constant folding of the shell's pure transforms (round 5 step 2, design §7.3.1): what a pipe of
 * literal text through decoders prints, computed here, so a name built at run time (`$(echo LmVudg== |
 * base64 -d)`, `$(printf '\x2e\x65\x6e\x76')`, `$(echo vne. | rev)`, `$(echo 2e656e76 | xxd -r -p)`) is
 * the name it makes. One table, `TRANSFORMS`: a program, the options that make it the transform, and the
 * function. Anything else (a file read, an unknown word, a transform not in the table) is not folded.
 */

/** The most text folded, in characters: past it, the result is not computed. */
const MAX_FOLD = 4096;

function rot13(text: string): string {
  return text.replace(/[A-Za-z]/g, (c) => String.fromCharCode(((c.charCodeAt(0) - (c <= "Z" ? 65 : 97) + 13) % 26) + (c <= "Z" ? 65 : 97)));
}

/** `tr SET1 SET2` for literal sets with ranges (`a-z`, `A-Za-z`), and `-d SET`. */
function tr(args: readonly string[], text: string): string | undefined {
  const expand = (set: string) => {
    const out: string[] = [];
    for (let i = 0; i < set.length; i++) {
      if (set[i + 1] === "-" && i + 2 < set.length) {
        for (let c = set.charCodeAt(i); c <= set.charCodeAt(i + 2); c++) out.push(String.fromCharCode(c));
        i += 2;
      } else out.push(set[i]);
    }
    return out;
  };
  if (args[0] === "-d" && args.length === 2) {
    const drop = new Set(expand(args[1]));
    return [...text].filter((c) => !drop.has(c)).join("");
  }
  if (args.length !== 2 || args.some((arg) => arg.startsWith("-") || /\[|\\/.test(arg))) return undefined;
  const from = expand(args[0]);
  const to = expand(args[1]);
  if (!to.length) return undefined;
  const map = new Map(from.map((c, i) => [c, to[Math.min(i, to.length - 1)]]));
  return [...text].map((c) => map.get(c) ?? c).join("");
}

/** A `sed` given one substitution (`s#a#b#`, `-e s/a/b/g`) and nothing else (iteration 4). */
function sedScript(args: readonly string[]): string | undefined {
  const scripts = args[0] === "-e" ? args.slice(1) : args;
  return scripts.length === 1 && /^s(.).*\1.*\1g?$/s.test(scripts[0]) ? scripts[0] : undefined;
}

/** A substitution applied line by line, its pattern a basic regular expression without groups or intervals. */
function sedApply(script: string, text: string): string | undefined {
  const delimiter = script[1];
  const parts = script.slice(2).split(delimiter);
  if (parts.length !== 3 || !/^g?$/.test(parts[2])) return undefined;
  const [pattern, replacement] = parts;
  if (/[\\(){}|+?]/.test(pattern) || /\\/.test(replacement)) return undefined;
  let regex: RegExp;
  try {
    regex = new RegExp(pattern, parts[2] === "g" ? "g" : "");
  } catch {
    return undefined;
  }
  const value = replacement.replace(/\$/g, "$$$$").replace(/&/g, "$$&");
  return text.split("\n").map((line) => line.replace(regex, value)).join("\n");
}

/** The transforms (data): a program, when its words make it one, and what it makes of its input. */
export const TRANSFORMS: readonly { program: string; when: (args: readonly string[]) => boolean; apply: (text: string, args: readonly string[]) => string | undefined }[] = [
  { program: "base64", when: (args) => args.some((arg) => /^(-d|-D|--decode)$/.test(arg)), apply: (text) => Buffer.from(text.replace(/\s+/g, ""), "base64").toString("utf8") },
  { program: "base64", when: (args) => args.every((arg) => /^(-w\d*|--wrap=\d+|-i|-)$/.test(arg)), apply: (text) => `${Buffer.from(text, "utf8").toString("base64")}\n` },
  { program: "base32", when: (args) => args.some((arg) => /^(-d|--decode)$/.test(arg)), apply: () => undefined },
  { program: "xxd", when: (args) => args.includes("-r") && args.includes("-p"), apply: (text) => Buffer.from(text.replace(/\s+/g, ""), "hex").toString("utf8") },
  { program: "xxd", when: (args) => args.includes("-r") && args.includes("-ps"), apply: (text) => Buffer.from(text.replace(/\s+/g, ""), "hex").toString("utf8") },
  { program: "rev", when: (args) => args.length === 0, apply: (text) => text.split("\n").map((line) => [...line].reverse().join("")).join("\n") },
  { program: "tr", when: () => true, apply: (text, args) => (args.length === 2 && /^(A-Za-z|a-zA-Z)$/.test(args[0]) && /^(N-ZA-Mn-za-m|n-za-mN-ZA-M)$/.test(args[1]) ? rot13(text) : tr(args, text)) },
  { program: "sed", when: (args) => sedScript(args) !== undefined, apply: (text, args) => sedApply(sedScript(args)!, text) },
  { program: "cat", when: (args) => args.length === 0, apply: (text) => text },
  { program: "tee", when: (args) => args.length === 0, apply: (text) => text },
  { program: "head", when: (args) => args.length === 0, apply: (text) => text },
  { program: "cut", when: () => false, apply: () => undefined },
];

/** What the folding knows of the machine (final pass): the user the line runs as, whose name `whoami` prints. */
export interface FoldContext {
  user?: string;
}

/**
 * Commands that print the name of the user the line runs as (final pass, data): `whoami`, `logname`, `id -un`.
 * `~$(whoami)` after an `eval` is the home folder; the name is no secret, but what it builds is a place.
 */
const USER_PRINTERS: readonly { program: string; when: (args: readonly string[]) => boolean }[] = [
  { program: "whoami", when: (args) => args.length === 0 },
  { program: "logname", when: (args) => args.length === 0 },
  { program: "id", when: (args) => args.length > 0 && args.every((arg) => /^-(un|nu|u|n)$/.test(arg)) && args.join("").includes("u") && args.join("").includes("n") },
];

/**
 * Commands that print a path they are given, resolved (final pass, data): `realpath X`, `readlink -f X`, `grealpath`,
 * Folded to X itself; where X really leads (symlinks, `..`) is resolved where the path is
 * classified, as any literal path is. `basename`/`dirname` print a part of it.
 */
const PATH_PRINTERS: readonly { program: string; when: (args: readonly string[]) => boolean; apply: (operand: string) => string }[] = [
  { program: "realpath", when: (args) => args.every((arg) => /^-[esmqPL]+$/.test(arg) || !arg.startsWith("-")), apply: (operand) => operand },
  { program: "grealpath", when: (args) => args.every((arg) => /^-[esmqPL]+$/.test(arg) || !arg.startsWith("-")), apply: (operand) => operand },
  { program: "readlink", when: (args) => args.some((arg) => /^-[a-zA-Z]*[fem]/.test(arg) || /^--canonicalize/.test(arg)), apply: (operand) => operand },
  { program: "basename", when: (args) => args.every((arg) => !arg.startsWith("-")) && args.length === 1, apply: (operand) => operand.replace(/\/+$/, "").split("/").pop() ?? operand },
  { program: "dirname", when: (args) => args.every((arg) => !arg.startsWith("-")) && args.length === 1, apply: (operand) => operand.replace(/\/+$/, "").replace(/\/[^/]*$/, "") || (operand.startsWith("/") ? "/" : ".") },
];

/** The literal text a command prints of its own (`echo`, `printf` with `\x`/octal escapes, `echo -e`, a user-name printer), or undefined. */
export function printed(words: readonly string[], unknown: string, context: FoldContext = {}): string | undefined {
  const [program, ...args] = words;
  if (program === undefined || words.includes(unknown)) return undefined;
  if (context.user && USER_PRINTERS.some((row) => row.program === program && row.when(args))) return `${context.user}\n`;
  const pathPrinter = PATH_PRINTERS.find((row) => row.program === program && row.when(args));
  if (pathPrinter) {
    const operands = args.filter((arg) => !arg.startsWith("-"));
    if (operands.length === 1 && operands[0] !== "") return `${pathPrinter.apply(operands[0])}\n`;
  }
  const unescape = (text: string) =>
    text
      .replace(/\\x([0-9a-fA-F]{1,2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)))
      .replace(/\\0?([0-7]{1,3})/g, (_, oct) => String.fromCharCode(parseInt(oct, 8)))
      .replace(/\\n/g, "\n")
      .replace(/\\t/g, "\t")
      .replace(/\\\\/g, "\\");
  if (program === "echo") {
    const flags = args.length && /^-[neE]+$/.test(args[0]) ? args[0] : "";
    const rest = flags ? args.slice(1) : args;
    const text = rest.join(" ");
    return `${flags.includes("e") ? unescape(text) : text}${flags.includes("n") ? "" : "\n"}`;
  }
  // `jq -rn '"…"'` (iteration 3): a JSON string literal printed raw.
  if (program === "jq" && args.some((arg) => /^-[a-zA-Z]*n/.test(arg)) && args.some((arg) => /^-[a-zA-Z]*r/.test(arg))) {
    const filter = args.filter((arg) => !arg.startsWith("-"));
    if (filter.length === 1 && /^"(?:[^"\\]|\\.)*"$/.test(filter[0])) {
      try {
        return `${JSON.parse(filter[0])}\n`;
      } catch {}
    }
  }
  if (program === "printf" && args.length && !args[0].startsWith("-")) {
    let used = 1;
    // `%b` expands the escapes of its argument (iteration 3), `%s` takes it as it is.
    const text = unescape(args[0]).replace(/%[sbd]/g, (spec) => (spec === "%b" ? unescape(args[used++] ?? "") : (args[used++] ?? "")));
    return `${text}${args.slice(used).join(" ")}`;
  }
  return undefined;
}

/**
 * What a pipe prints, folded: a literal printer (or literal input) through transforms of `TRANSFORMS`.
 * `chain`: the pipe's commands in order, each with its words and its literal input (a here-string).
 */
export function foldPipe(chain: readonly { words: readonly string[]; input?: string }[], unknown: string, context: FoldContext = {}): string | undefined {
  if (!chain.length) return undefined;
  const [first, ...rest] = chain;
  let text = first.input !== undefined && first.words.length && TRANSFORMS.some((row) => row.program === first.words[0]) ? applyOne(first.words, first.input, unknown) : printed(first.words, unknown, context);
  for (const command of rest) {
    if (text === undefined || text.length > MAX_FOLD) return undefined;
    text = applyOne(command.words, text, unknown);
  }
  return text === undefined || text.length > MAX_FOLD ? undefined : text;
}

function applyOne(words: readonly string[], text: string, unknown: string): string | undefined {
  const [program, ...args] = words;
  if (program === undefined || words.includes(unknown)) return undefined;
  const name = program.split("/").pop() ?? program;
  const row = TRANSFORMS.find((candidate) => candidate.program === name && candidate.when(args));
  return row ? row.apply(text, args) : undefined;
}
