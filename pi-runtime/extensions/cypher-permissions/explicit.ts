/**
 * Explicit requests: a dangerous command (publishing, pushing, a pull
 * request) runs without a dialog when the owner's own words ask for exactly
 * it. The reviewing model finds those words and names what the command acts
 * on; this module checks both, so the model's say-so is never enough:
 *
 * 1. The quote is the owner's: it occurs, as written (whitespace, case and
 *    full-width punctuation aside), in one of the owner's own messages. Only
 *    the words the engine passes through `CYPHER_OWNER_WORDS` count: what a
 *    person typed in this chat, and a plan's instruction from its record.
 *    Text an agent wrote (a plan's brief, an orchestrator's steering, a
 *    subagent's task, a peer's question) never does, even when it sits in
 *    the conversation as a user message. A bare "continue" or "ok" is too
 *    vague to ask for anything.
 * 2. The target is the command's: read from the line itself (the pull
 *    request `gh pr merge 52` merges, the tag or branch `git push origin x`
 *    publishes, the package `npm publish` publishes, the host `curl` or
 *    `ssh` reaches), it must be what the model named, and what the owner's
 *    message names or plainly implies (a release tag on the default branch
 *    holding the version, for "release a new version"; the pull request
 *    this chat opened, for "open a PR and merge it").
 * 3. Nothing the owner wrote since takes it back or moves on to other work,
 *    and the message itself doesn't say not to ("open a PR, don't merge it").
 *    A later "don't" about something else ("don't forget the changelog")
 *    takes nothing back.
 *
 * A target known only when the line runs (`git push origin $BRANCH`) can't
 * be checked, so it asks; a body or title known only when it runs
 * (`--body "$(cat …)"`) can be, since it changes what is said, not where.
 */

import { basename, isAbsolute, join, resolve } from "node:path";

import { remoteOperand, sshHostIndex } from "./commands.ts";
import type { GitFacts } from "./risk.ts";
import { UNKNOWN } from "./shell.ts";

/** The variable the harness sets when it spawns Pi (`crates/harness/src/pi/spawn.rs`). */
export const OWNER_WORDS_ENV = "CYPHER_OWNER_WORDS";

/** How many of the owner's messages count, newest last. */
const MAX_MESSAGES = 10;
/** How much of one message counts. */
const MAX_MESSAGE_CHARS = 4000;

/** One message in the owner's own words. */
export interface OwnerWord {
  text: string;
  /** Typed in this chat, or the instruction a plan carries out (from its record). */
  source: "typed" | "plan";
}

/** What the engine knows the owner wrote: the only words that can ask for a danger. */
export interface OwnerWords {
  /** Oldest first. */
  messages: OwnerWord[];
  /** Some of the chat's user messages were written by an agent (a brief, a steer, a subagent's task). */
  agentWritten: boolean;
}

/** The owner's words as the harness passed them; undefined when it passed none (an older Cypher, or no Cypher at all). */
export function parseOwnerWords(raw: string | undefined): OwnerWords | undefined {
  if (!raw) return undefined;
  try {
    const value = JSON.parse(raw) as { messages?: unknown; agentWritten?: unknown };
    if (!Array.isArray(value.messages)) return undefined;
    const messages: OwnerWord[] = [];
    for (const item of value.messages as { text?: unknown; source?: unknown }[]) {
      if (typeof item?.text !== "string" || !item.text.trim()) continue;
      messages.push({ text: item.text.slice(0, MAX_MESSAGE_CHARS), source: item.source === "plan" ? "plan" : "typed" });
    }
    return { messages: messages.slice(-MAX_MESSAGES), agentWritten: value.agentWritten === true };
  } catch {
    return undefined;
  }
}

// --- Text ------------------------------------------------------------------------

/** Text as the quote check compares it: compatibility forms (full-width), lower case, no whitespace, punctuation or symbols. */
export function normalize(text: string): string {
  return text.normalize("NFKC").toLowerCase().replace(/[\s\p{P}\p{S}]+/gu, "");
}

/** How much a normalized quote says: a CJK character counts twice. */
function weight(normalized: string): number {
  let total = 0;
  for (const char of normalized) total += /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u.test(char) ? 2 : 1;
  return total;
}

/** The least a quote must say: "pr 给main" passes, "继续" and "ok" don't. */
const MIN_QUOTE_WEIGHT = 6;

/** Words that say to go on without saying what (normalized). */
const VAGUE = new Set(
  [
    "continue", "continue please", "please continue", "go ahead", "go on", "go", "ok", "okay", "yes", "yep", "sure", "proceed", "do it", "carry on",
    "ship it", "lgtm", "fine", "approved", "approve", "sounds good", "looks good", "keep going", "next", "try again", "all set", "push it",
    "push this", "push that", "merge it", "release it", "publish it", "do the release", "go for it", "just do it",
    "继续", "继续吧", "好", "好的", "可以", "可以了", "行", "没问题", "同意", "批准", "开始", "搞吧", "发布吧", "发布", "推送", "推送吧", "推吧",
    "推送先", "先推送", "合并", "合并吧", "发吧", "上吧", "再试试", "再试一次", "好了", "好了继续", "push 这个", "推这个", "推送这个",
    "合并这个", "发布这个",
  ].map(normalize),
);

/** A later message that takes a request back or changes it ("先别 push", "don't merge", "wait"). */
const RETRACTION =
  /\b(wait|hold (on|off)|don'?t|do not|stop|cancel|abort|never ?mind|not yet|no longer|revert|undo|instead|scratch that|forget (it|that))\b|先别|别(?:再|急|动|推|合|发|做|弄|改|管|push|merge)|不要|不用|先停|停下|停一下|等等|等一下|等下|取消|算了|暂停|先不|撤销|回滚|改成|改为|换成/i;

/** Words that say not to do what follows them ("don't merge", "先别合并"). */
const NEGATION = /\b(?:don'?t|do not|never|not yet|no need to|without|instead of|hold off on|skip)\b|先别|别|不要|不用|先不|不必|无需|不需要/gi;

/** Words a bare "wait" or "don't" may come with and still mean the request at hand ("don't do that yet", "先别了"). */
const FILLER =
  /\b(?:it|that|this|them|those|these|yet|now|for now|please|pls|just|ok(?:ay)?|so|and|but|oh|hey|actually|no|nope|sorry|oops|well|hmm+|a (?:sec|second|moment|minute|bit)|do|doing|anything|any of (?:it|that)|one|more|on|up|the|all)\b|这个|那个|这些|那些|它|一下|吧|了|先|请|再|啊|哦|呢|做|弄|搞/gi;

/** A "don't" that asks for the thing ("don't forget to merge", "别忘了合并"). */
const REMINDER = /\b(?:don'?t|do not|never)\s+(?:forget|hesitate|worry|be afraid)\b|(?:别|不要|不用)(?:忘了?|忘记|担心|犹豫)/gi;

/** The most a clause may say besides its "wait" and still be a bare one. */
const BARE_WEIGHT = 4;

/** A message's sentences: a "don't" belongs to the sentence it stands in. */
function sentences(text: string): string[] {
  return text
    .split(/[.!?](?=\s|$)|[;。！？；\n]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}

/** The words that ask for what a target's command does, for telling what a "don't" is about. */
function verbOf(target: Target): RegExp {
  if (target.kind === "pr") return target.action === "create" ? /open|create|make|file|raise|submit|send|开|提|建|发/i : (SAYS[target.action as keyof typeof SAYS] ?? SAYS.pr);
  if (target.kind === "branch") return target.action === "delete" ? SAYS.delete : SAYS.push;
  if (target.kind === "tag") return /release|publish|ship|tag|push|发布|发版|上线|标签|推/i;
  if (target.kind === "package") return target.action === "install" ? SAYS.install : SAYS.publish;
  return /deploy|ssh|connect|log ?in|run|登录|连接|部署|执行/i;
}

/** Whether a sentence is about what a target's command does: it names the target, or its action. */
function concerns(text: string, target: Target): boolean {
  if (mentions(text, target.value) || verbOf(target).test(text)) return true;
  switch (target.kind) {
    case "pr":
      return SAYS.pr.test(text) || SAYS.merge.test(text);
    case "branch":
      return /branch|分支/i.test(text);
    case "tag":
      return SAYS.release.test(text);
    case "package":
      return SAYS.publish.test(text) || SAYS.install.test(text);
    case "host":
      return /server|host|machine|cluster|服务器|主机|集群/i.test(text);
  }
}

/**
 * Whether a later message takes a request back: a sentence with a "wait",
 * "don't" or "instead" that is about what the command does, or that says
 * nothing else ("wait", "先别", "don't do that yet"). A "don't" about
 * something else ("don't rename any branch", for a merge) takes nothing back.
 */
function retracts(text: string, targets: readonly Target[]): boolean {
  return sentences(text.normalize("NFKC").replace(REMINDER, " ")).some((sentence) => {
    if (!RETRACTION.test(sentence)) return false;
    if (targets.some((target) => concerns(sentence, target))) return true;
    const rest = sentence.replace(new RegExp(RETRACTION.source, "gi"), " ").replace(FILLER, " ");
    return weight(normalize(rest)) <= BARE_WEIGHT;
  });
}

/** Whether a message says not to do what a target's command does: a "don't" right before its verb ("open a PR, don't merge it"). */
function forbids(text: string, target: Target): boolean {
  const verb = verbOf(target);
  const haystack = text.normalize("NFKC").replace(REMINDER, " ");
  for (const match of haystack.matchAll(NEGATION)) {
    const after = haystack.slice(match.index + match[0].length, match.index + match[0].length + 32).split(/[,.;!?，。；！？、\n]/)[0];
    // The verb must come right after: within a couple of words.
    const near = /[\p{Script=Han}]/u.test(after) ? after.slice(0, 8) : after.trim().split(/\s+/).slice(0, 3).join(" ");
    if (verb.test(near)) return true;
  }
  return false;
}

/** A later message that only carries on ("好了 继续发布", "try again", "looks good"). */
const CONTINUATION =
  /^(ok(ay)?|yes|yep|yeah|sure|go( ahead| on)?|continue|proceed|carry on|keep going|try (it )?again|retry|again|all set|done|fixed|ready|looks good|lgtm|sounds good|fine|good|great|perfect|thanks?|thank you|继续|好|嗯|对|是|可以|行|没问题|再试|重试|试试|再来|搞定|弄好|修好|完成|谢谢)/i;
const CONTINUES = /continue|go on|carry on|继续|接着/i;

// --- Targets ---------------------------------------------------------------------

export type TargetKind = "pr" | "branch" | "tag" | "package" | "host";

/** What one dangerous command acts on, read from the line. */
export interface Target {
  kind: TargetKind;
  /** The pull request's number (or branch), the branch, the tag, `name@version`, the host. */
  value: string;
  /** What it does there: `merge`, `create`, `push`, `release`, `publish`, `install`, `reach`, `send`. */
  action: string;
  /** The repository (`owner/name`) a `gh` command names with `-R` or a URL. */
  repo?: string;
  /** The remote a push goes to. */
  remote?: string;
  /** A new pull request's base and head. */
  base?: string;
  head?: string;
  /** The commit a tag marks, when the line says (undefined: known only when it runs). */
  commit?: string;
  /** A package's name and version. */
  name?: string;
  version?: string;
  /** Where the command runs (for git facts). */
  dir?: string;
}

/** A line's targets, or why one can't be read before it runs. */
export type LineTargets = { targets: Target[] } | { unknown: string };

/** What reading targets needs to know about the machine. */
export interface TargetFacts {
  git(dir: string): GitFacts | undefined;
  /** A tag of that name exists in the repository at `dir`. */
  tagExists(dir: string, name: string): boolean;
  /** A file's text, or undefined. */
  readFile(path: string): string | undefined;
}

/** One command of a line, as core.ts reads it. */
export interface TargetCommand {
  words: readonly string[];
  cwd: string | undefined;
  /** The classifier found it dangerous. */
  danger: boolean;
}

const VERSION = /(?<![\w.])v?(\d+\.\d+(?:\.\d+)*)(?![\d])/g;

function versionOf(name: string): string | undefined {
  const match = /(\d+\.\d+(?:\.\d+)*)/.exec(name);
  return match?.[1];
}

/** git's global options, and the directory `-C` moves to. */
function gitParts(words: readonly string[], cwd: string | undefined): { sub?: string; rest: string[]; dir: string | undefined } {
  let dir = cwd;
  let i = 1;
  for (; i < words.length; i++) {
    const word = words[i];
    if (word === "-C") {
      const next = words[++i];
      if (next === undefined || next === UNKNOWN) dir = undefined;
      else dir = isAbsolute(next) ? next : dir === undefined ? undefined : resolve(dir, next);
    } else if (word === "-c" || word === "--git-dir" || word === "--work-tree" || word === "--namespace") i++;
    else if (!word.startsWith("-")) break;
  }
  return { sub: words[i], rest: words.slice(i + 1), dir };
}

/** Options of `gh` that take a value (so the value is no positional). */
const GH_VALUES = new Set([
  "-R", "--repo", "-B", "--base", "-H", "--head", "-t", "--title", "-b", "--body", "-F", "--body-file", "-a", "--assignee", "-l", "--label",
  "-m", "--milestone", "-r", "--reviewer", "-p", "--project", "-T", "--template", "--subject", "-A", "--author-email", "--match-head-commit",
  "-n", "--notes", "--notes-file", "--target", "--notes-start-tag", "--discussion-category", "-c", "--comment", "--add-label", "--remove-label",
  "--add-reviewer", "--remove-reviewer", "--add-assignee", "--remove-assignee", "--add-project", "--remove-project", "--hostname", "-q", "--jq",
  "--json", "--template", "-e",
]);

function ghParts(args: readonly string[]): { positionals: string[]; options: Map<string, string> } {
  const positionals: string[] = [];
  const options = new Map<string, string>();
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word !== UNKNOWN && word.startsWith("-") && word !== "-") {
      const eq = word.indexOf("=");
      if (word.startsWith("--") && eq > 0) options.set(word.slice(0, eq), word.slice(eq + 1));
      else if (GH_VALUES.has(word)) options.set(word, args[++i] ?? UNKNOWN);
      else options.set(word, "");
      continue;
    }
    positionals.push(word);
  }
  return { positionals, options };
}

function option(options: Map<string, string>, ...names: string[]): string | undefined {
  for (const name of names) if (options.has(name)) return options.get(name);
  return undefined;
}

/** `owner/name` from a GitHub URL or `-R` value (`host/owner/name` keeps the last two). */
export function repoName(text: string): string | undefined {
  const url = /github\.com[/:]([\w.-]+)\/([\w.-]+?)(?:\.git)?(?:[/#?]|$)/i.exec(text);
  if (url) return `${url[1]}/${url[2]}`.toLowerCase();
  const parts = text.replace(/\.git$/, "").split("/").filter(Boolean);
  return parts.length >= 2 ? `${parts.at(-2)}/${parts.at(-1)}`.toLowerCase() : undefined;
}

function packageOf(dir: string, program: string, facts: TargetFacts): { name: string; version: string } | undefined {
  if (program === "cargo") {
    const text = facts.readFile(join(dir, "Cargo.toml"));
    const section = text?.split(/^\[/m).find((part) => part.startsWith("package]"));
    const name = section && /^name\s*=\s*"([^"]+)"/m.exec(section)?.[1];
    const version = section && /^version\s*=\s*"([^"]+)"/m.exec(section)?.[1];
    return name && version ? { name, version } : undefined;
  }
  try {
    const json = JSON.parse(facts.readFile(join(dir, "package.json")) ?? "") as { name?: unknown; version?: unknown };
    return typeof json.name === "string" && typeof json.version === "string" ? { name: json.name, version: json.version } : undefined;
  } catch {
    return undefined;
  }
}

/** git subcommands that move HEAD: a tag of `HEAD` after one is known only when the line runs. */
const MOVES_HEAD = /^(commit|merge|reset|checkout|switch|pull|rebase|cherry-pick|am|revert|stash)$/;

/**
 * What a line's dangerous commands act on. Every one must be read before the
 * line runs; a line with a danger whose target this can't read gives why.
 */
export function lineTargets(commands: readonly TargetCommand[], facts: TargetFacts): LineTargets {
  const targets: Target[] = [];
  /** Tags the line creates, with the commit (undefined: known only when it runs). */
  const created = new Map<string, string | undefined>();
  /** Branches the line creates (`git switch -c x`). */
  const branches = new Set<string>();
  let headMoved = false;
  for (const command of commands) {
    const words = command.words;
    const program = basename(words[0] ?? "");
    if (program === "git") {
      const { sub, rest, dir } = gitParts(words, command.cwd);
      const made = (sub === "switch" || sub === "checkout") && rest.findIndex((word) => /^-[cCbB]$|^--create$|^--force-create$/.test(word));
      if (typeof made === "number" && made >= 0 && rest[made + 1] && rest[made + 1] !== UNKNOWN) branches.add(rest[made + 1]);
      if (sub === "tag") {
        const positionals: string[] = [];
        let listing = false;
        for (let i = 0; i < rest.length; i++) {
          const word = rest[i];
          if (word === "-m" || word === "-F" || word === "-u" || word === "--message" || word === "--file" || word === "--local-user") i++;
          else if (/^(-d|--delete|-l|--list|-v|--verify|-n\d*|--contains|--points-at|--merged|--no-merged|--sort.*|--format.*)$/.test(word)) listing = true;
          else if (word !== UNKNOWN && word.startsWith("-")) continue;
          else positionals.push(word);
        }
        if (!listing && positionals[0] !== undefined) {
          const commit = positionals[1] ?? "HEAD";
          created.set(positionals[0], commit === UNKNOWN || (headMoved && /^(HEAD|@)/.test(commit)) ? undefined : commit);
        }
        continue;
      }
      if (sub !== undefined && MOVES_HEAD.test(sub)) headMoved = true;
      if (!command.danger) continue;
      if (sub !== "push") return { unknown: `Cypher can't tell what \`git ${sub ?? ""}\` acts on` };
      const read = pushTargets(rest, dir, created, branches, facts);
      if ("unknown" in read) return read;
      targets.push(...read.targets);
      continue;
    }
    if (!command.danger) continue;
    if (words.includes(UNKNOWN) && !/^(gh)$/.test(program)) {
      // Only gh's titles and bodies may be unknown: they change what is said, not where.
      return { unknown: `\`${program}\` is given words known only when it runs` };
    }
    if (program === "gh") {
      const read = ghTarget(words.slice(1), command.cwd, facts);
      if ("unknown" in read) return read;
      targets.push(read.target);
      continue;
    }
    const install = installTargets(program, words.slice(1));
    if (install) {
      if (install.length === 0) return { unknown: `Cypher can't tell which packages \`${program}\` installs` };
      targets.push(...install);
      continue;
    }
    if (/^(npm|pnpm|yarn|bun|cargo)$/.test(program)) {
      const args = words.slice(1).filter((word) => !word.startsWith("-"));
      if (args[0] !== "publish") return { unknown: `Cypher can't tell what \`${program} ${args[0] ?? ""}\` acts on` };
      const where = args[1] && program !== "cargo" ? args[1] : undefined;
      const dir = where ? (isAbsolute(where) ? where : command.cwd && resolve(command.cwd, where)) : command.cwd;
      const pkg = dir ? packageOf(dir, program, facts) : undefined;
      if (!pkg) return { unknown: "Cypher can't read which package it publishes" };
      targets.push({ kind: "package", value: `${pkg.name}@${pkg.version}`, action: "publish", name: pkg.name, version: pkg.version, dir });
      continue;
    }
    if (/^(ssh|mosh|sftp)$/.test(program)) {
      const at = sshHostIndex(words);
      const host = at === undefined ? undefined : words[at];
      if (!host || host === UNKNOWN) return { unknown: "the host is known only when it runs" };
      targets.push({ kind: "host", value: host.replace(/^[^@]*@/, "").toLowerCase(), action: "reach" });
      continue;
    }
    if (/^(scp|rsync)$/.test(program)) {
      const hosts = new Set(words.slice(1).map((word) => remoteOperand(word)?.host).filter((host): host is string => host !== undefined));
      if (hosts.size === 0) return { unknown: "Cypher can't tell which host it reaches" };
      for (const host of hosts) targets.push({ kind: "host", value: host.toLowerCase(), action: "reach" });
      continue;
    }
    if (/^(curl|wget|http|https|xh)$/.test(program)) {
      const hosts = new Set<string>();
      for (const word of words.slice(1)) {
        const url = /^(?:[a-z][\w+.-]*:\/\/)(?:[^@/]*@)?([^/:?#]+)/i.exec(word);
        if (url) hosts.add(url[1].toLowerCase());
      }
      if (hosts.size === 0) return { unknown: "Cypher can't tell which server it reaches" };
      for (const host of hosts) targets.push({ kind: "host", value: host, action: "send" });
      continue;
    }
    return { unknown: `Cypher can't tell from your words what \`${program}\` acts on` };
  }
  return { targets };
}

/** What an install for the whole machine installs (`brew install rg`, `npm i -g pnpm`): undefined when it is no install. */
function installTargets(program: string, args: readonly string[]): Target[] | undefined {
  const positionals = args.filter((word) => !word.startsWith("-"));
  const verb = positionals[0];
  const installs =
    (/^(brew|port|pipx|gem|apt|apt-get|dnf|yum|pacman|snap)$/.test(program) && /^(install|reinstall|upgrade|-S)$/.test(verb ?? "")) ||
    (/^(npm|pnpm|bun)$/.test(program) && /^(i|install|add)$/.test(verb ?? "")) ||
    (program === "yarn" && verb === "global") ||
    (/^(cargo|go|uv)$/.test(program) && verb === "install") ||
    (/^pip3?$/.test(program) && verb === "install");
  if (!installs) return undefined;
  const names = positionals.slice(program === "yarn" || program === "uv" ? 2 : 1);
  if (names.some((name) => name === UNKNOWN)) return [];
  return names.map((name) => ({ kind: "package" as const, value: name, action: "install", name: name.replace(/(?<=.)@[^/]*$/, "") }));
}

function pushTargets(
  rest: readonly string[],
  dir: string | undefined,
  created: Map<string, string | undefined>,
  branches: ReadonlySet<string>,
  facts: TargetFacts,
): LineTargets {
  const positionals: string[] = [];
  let remote: string | undefined;
  let deletes = false;
  for (let i = 0; i < rest.length; i++) {
    const word = rest[i];
    if (word === UNKNOWN) return { unknown: "it pushes something known only when it runs" };
    if (word === "-o" || word === "--push-option" || word === "--receive-pack" || word === "--exec") {
      i++;
      continue;
    }
    if (word === "-d" || word === "--delete") deletes = true;
    if (word.startsWith("--repo=")) remote = word.slice("--repo=".length);
    else if (word.startsWith("-")) continue;
    else positionals.push(word);
  }
  remote ??= positionals.shift();
  if (remote === undefined) return { unknown: "it pushes to the branch's upstream, which the line doesn't name" };
  if (positionals.length === 0) return { unknown: "it pushes what the remote's settings say, which the line doesn't name" };
  const git = dir === undefined ? undefined : facts.git(dir);
  const targets: Target[] = [];
  // `git push origin --delete x`: the refs it names are the remote's, and
  // whether they still exist here says nothing.
  if (deletes) {
    for (const refspec of positionals) {
      const name = refspec.replace(/^refs\/(heads|tags)\//, "");
      const tagged = refspec.startsWith("refs/tags/") || (dir !== undefined && facts.tagExists(dir, name));
      targets.push({ kind: tagged ? "tag" : "branch", value: name, action: "delete", remote, dir });
    }
    return { targets };
  }
  for (const refspec of positionals) {
    const [source, dest = source] = refspec.replace(/^\+/, "").split(":");
    const name = dest.replace(/^refs\/(heads|tags)\//, "");
    const tagged = dest.startsWith("refs/tags/") || source.startsWith("refs/tags/");
    if (tagged || created.has(name)) {
      targets.push({ kind: "tag", value: name, action: "push", remote, commit: created.has(name) ? created.get(name) : name, dir });
      continue;
    }
    const branch = name === "HEAD" ? git?.current : name;
    if (branch && ((git && (git.isBranch(branch) || refspec.includes(":"))) || branches.has(branch))) {
      targets.push({ kind: "branch", value: branch, action: "push", remote, dir });
      continue;
    }
    if (dir !== undefined && facts.tagExists(dir, name)) {
      targets.push({ kind: "tag", value: name, action: "push", remote, commit: name, dir });
      continue;
    }
    return { unknown: `Cypher can't tell whether \`${name}\` is a branch or a tag` };
  }
  return { targets };
}

function ghTarget(args: readonly string[], cwd: string | undefined, facts: TargetFacts): { target: Target } | { unknown: string } {
  const { positionals, options } = ghParts(args);
  const [group, action, subject] = positionals;
  const repoOption = option(options, "-R", "--repo");
  if (repoOption === UNKNOWN) return { unknown: "the repository is known only when it runs" };
  let repo = repoOption ? repoName(repoOption) : undefined;
  if (group === UNKNOWN || action === UNKNOWN) return { unknown: "what gh does is known only when it runs" };
  if (group === "pr" && action === "create") {
    const git = cwd === undefined ? undefined : facts.git(cwd);
    const base = option(options, "-B", "--base") ?? git?.defaults[0];
    const head = option(options, "-H", "--head") ?? git?.current;
    if (base === undefined || base === UNKNOWN) return { unknown: "the pull request's base is known only when it runs" };
    if (head === UNKNOWN) return { unknown: "the pull request's branch is known only when it runs" };
    return { target: { kind: "pr", value: `${head ?? "this branch"} → ${base}`, action: "create", base, head, repo, dir: cwd } };
  }
  if (group === "pr" && /^(merge|close|comment|review|edit|ready|reopen)$/.test(action ?? "")) {
    if (subject === undefined) return { unknown: "it acts on the current branch's pull request, whose number the line doesn't name" };
    if (subject === UNKNOWN) return { unknown: "the pull request is known only when it runs" };
    const url = /\/pull\/(\d+)/.exec(subject);
    if (url) repo ??= repoName(subject);
    const number = url?.[1] ?? (/^#?\d+$/.test(subject) ? subject.replace("#", "") : undefined);
    return { target: { kind: "pr", value: number ?? subject, action: action!, repo, dir: cwd } };
  }
  if (group === "release" && /^(create|edit|upload)$/.test(action ?? "")) {
    if (subject === undefined || subject === UNKNOWN) return { unknown: "the release's tag is known only when it runs" };
    const target = option(options, "--target");
    return { target: { kind: "tag", value: subject, action: "release", repo, commit: target && target !== UNKNOWN ? target : subject, dir: cwd } };
  }
  return { unknown: `Cypher can't tell from your words what \`gh ${group ?? ""} ${action ?? ""}\` acts on` };
}

export function describeTarget(target: Target): string {
  switch (target.kind) {
    case "pr":
      return target.action === "create"
        ? `a new pull request from ${target.head ?? "this branch"} into ${target.base}${target.repo ? ` of ${target.repo}` : ""}`
        : `pull request #${target.value} (${target.action})${target.repo ? ` of ${target.repo}` : ""}`;
    case "branch":
      return `branch ${target.value} on ${target.remote}${target.action === "delete" ? " (deleted)" : ""}`;
    case "tag":
      return `tag ${target.value}${target.commit && target.commit !== target.value ? ` at ${target.commit}` : ""}${target.remote ? ` on ${target.remote}` : ""}${target.action === "release" ? " (a release)" : ""}`;
    case "package":
      return target.action === "install" ? `package ${target.value} (installed for the whole machine)` : `package ${target.value}`;
    case "host":
      return `host ${target.value}`;
  }
}

// --- The check -------------------------------------------------------------------

/** What the reviewing model claimed besides its decision. */
export interface ExplicitClaim {
  quote?: string;
  targets?: { kind: string; value: string }[];
}

/** Facts the check needs beyond the line. */
export interface ExplicitFacts {
  /** The `owner/name` of the repository `origin` points at, in `dir`. */
  originRepo(dir: string): string | undefined;
  /** A tag's commit is on the remote's default branch, and its files hold `version`. Undefined: can't tell. */
  release(dir: string, commit: string, version: string): { onDefault?: boolean; hasVersion?: boolean };
}

type Entry = { type?: string; message?: { role?: string; content?: unknown; toolCallId?: unknown; isError?: unknown } };

/** Pull requests this chat opened: a `gh pr create` whose result names exactly one. */
export function openedPullRequests(entries: readonly Entry[]): { number: string; repo?: string }[] {
  const creates = new Set<string>();
  const opened: { number: string; repo?: string }[] = [];
  for (const entry of entries) {
    const message = entry.type === "message" ? entry.message : undefined;
    if (!message) continue;
    if (message.role === "assistant" && Array.isArray(message.content)) {
      for (const part of message.content as { type?: string; id?: unknown; arguments?: { command?: unknown } }[]) {
        if (part?.type === "toolCall" && typeof part.id === "string" && typeof part.arguments?.command === "string" && /\bgh\s+pr\s+create\b/.test(part.arguments.command)) creates.add(part.id);
      }
    } else if (message.role === "toolResult" && typeof message.toolCallId === "string" && creates.has(message.toolCallId) && message.isError !== true) {
      const text = Array.isArray(message.content)
        ? (message.content as { type?: string; text?: unknown }[]).map((part) => (part?.type === "text" && typeof part.text === "string" ? part.text : "")).join("\n")
        : typeof message.content === "string"
          ? message.content
          : "";
      const found = new Map<string, string | undefined>();
      for (const match of text.matchAll(/github\.com\/([\w.-]+\/[\w.-]+)\/pull\/(\d+)/g)) found.set(match[2], match[1].toLowerCase());
      if (found.size === 1) {
        const [[number, repo]] = [...found];
        opened.push({ number, repo });
      }
    }
  }
  return opened;
}

function userTexts(entries: readonly Entry[]): string[] {
  const out: string[] = [];
  for (const entry of entries) {
    if (entry.type !== "message" || entry.message?.role !== "user") continue;
    const content = entry.message.content;
    const text = typeof content === "string" ? content : Array.isArray(content) ? (content as { type?: string; text?: unknown }[]).map((part) => (part?.type === "text" && typeof part.text === "string" ? part.text : "")).join("\n") : "";
    if (text.trim()) out.push(text);
  }
  return out;
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** `value` appears in `text` as a name of its own, not inside a longer one. */
export function mentions(text: string, value: string): boolean {
  if (!value) return false;
  const haystack = text.normalize("NFKC");
  return new RegExp(`(?<![A-Za-z0-9_./-])${escapeRegex(value.normalize("NFKC"))}(?![A-Za-z0-9_/-]|\\.[A-Za-z0-9])`, "i").test(haystack);
}

/** Pull request numbers a message names: `#52`, `PR 52`, `pull/52`. */
function namedPullRequests(text: string): Set<string> {
  const out = new Set<string>();
  const normalized = text.normalize("NFKC");
  for (const match of normalized.matchAll(/(?:#|\bpr\s*#?|\bmr\s*#?|pull\s*request\s*#?|\/pull\/|合并请求\s*#?)\s*(\d+)\b/gi)) out.add(match[1]);
  for (const match of normalized.matchAll(/\b(\d+)\s*号?\s*(?:pr|pull request)\b/gi)) out.add(match[1]);
  return out;
}

function namedVersions(text: string): Set<string> {
  return new Set([...text.normalize("NFKC").matchAll(VERSION)].map((match) => match[1]));
}

const SAYS = {
  pr: /\bpr\b|\bprs\b|pull\s*request|\bmr\b|合并请求|拉取请求/i,
  merge: /merge|合并|合入|合进|并入/i,
  close: /close|关闭|关掉/i,
  comment: /comment|reply|评论|留言|回复/i,
  review: /review|approve|审查|审核|批准/i,
  edit: /edit|title|body|description|修改|改/i,
  ready: /ready|draft|就绪/i,
  reopen: /reopen|重新打开|重开/i,
  push: /push|推送|推到|推上|推一下|推|上传/i,
  release: /release|publish|ship|tag|发布|发版|发新版|上线|打\s*tag|打标签|标签/i,
  version: /version|版本|新版|release/i,
  publish: /publish|release|发布|发版|上传/i,
  install: /install|安装|装上|装一下|装个/i,
  delete: /delete|remove|drop|prune|clean ?up|tidy|删除|删掉|删了|移除|清理|清除/i,
};

/** Why `message` (the owner's request) doesn't cover `target`; undefined when it does. */
function uncovered(target: Target, message: string, opened: readonly { number: string; repo?: string }[], facts: ExplicitFacts): string | undefined {
  const origin = target.dir ? facts.originRepo(target.dir) : undefined;
  if (target.repo && origin && target.repo !== origin && !mentions(message, target.repo) && !mentions(message, target.repo.split("/")[1] ?? "")) {
    return `it reaches ${target.repo}, a repository your message doesn't name`;
  }
  if (target.remote && target.remote !== "origin" && !mentions(message, target.remote)) return `it pushes to ${target.remote}, a remote your message doesn't name`;
  switch (target.kind) {
    case "pr": {
      if (target.action === "create") {
        if (!SAYS.pr.test(message)) return "your message doesn't ask for a pull request";
        if (target.base && !mentions(message, target.base) && !["main", "master"].includes(target.base)) return `it targets ${target.base}, a branch your message doesn't name`;
        return undefined;
      }
      const verb = SAYS[target.action as keyof typeof SAYS];
      if (!verb || !verb.test(message)) return `your message doesn't ask to ${target.action} a pull request`;
      const named = namedPullRequests(message);
      if (named.size) return named.has(target.value) ? undefined : `your message names pull request ${[...named].map((n) => `#${n}`).join(", ")}, not #${target.value}`;
      if (!SAYS.pr.test(message)) return `your message doesn't say which pull request (#${target.value})`;
      const ours = opened.filter((pr) => pr.number === target.value && (!pr.repo || !origin || pr.repo === origin) && (!target.repo || !pr.repo || pr.repo === target.repo));
      return ours.length ? undefined : `your message doesn't name pull request #${target.value}, and this chat didn't open it`;
    }
    case "branch":
      if (target.action === "delete") {
        if (!SAYS.delete.test(message)) return "your message doesn't ask to delete a branch";
        return mentions(message, target.value) ? undefined : `it deletes ${target.value}, a branch your message doesn't name`;
      }
      if (!SAYS.push.test(message)) return "your message doesn't ask to push";
      return mentions(message, target.value) ? undefined : `it pushes to ${target.value}, a branch your message doesn't name`;
    case "tag": {
      if (!SAYS.release.test(message) && !SAYS.push.test(message)) return "your message doesn't ask for a release or a tag";
      if (mentions(message, target.value)) return undefined;
      const version = versionOf(target.value);
      const named = namedVersions(message);
      if (named.size) return version && named.has(version) ? undefined : `your message names version ${[...named].join(", ")}, not ${target.value}`;
      if (!SAYS.release.test(message) || !SAYS.version.test(message)) return `your message doesn't name ${target.value} or ask for a new version`;
      if (!version) return `${target.value} carries no version to match a release against`;
      if (!target.dir || !target.commit) return `the commit ${target.value} marks is known only when it runs`;
      const release = facts.release(target.dir, target.commit, version);
      if (release.onDefault !== true) return `${target.value} doesn't mark a commit on the default branch`;
      if (release.hasVersion !== true) return `the commit ${target.value} marks doesn't hold version ${version}`;
      return undefined;
    }
    case "package": {
      if (target.action === "install") {
        if (!SAYS.install.test(message)) return "your message doesn't ask to install anything";
        return target.name && mentions(message, target.name) ? undefined : `it installs ${target.value}, which your message doesn't name`;
      }
      if (!SAYS.publish.test(message)) return "your message doesn't ask to publish";
      if (target.name && mentions(message, target.name)) return undefined;
      if (target.version && namedVersions(message).has(target.version)) return undefined;
      return `your message names neither ${target.name} nor version ${target.version}`;
    }
    case "host": {
      // By its name, its first label (`web` for `web.example.com`), or what its alias starts with (`gamma` for `gamma-cluster`).
      const first = target.value.split(".")[0];
      const lead = first.split(/[-_]/)[0];
      const named = mentions(message, target.value) || (first.length >= 3 && mentions(message, first)) || (lead !== first && lead.length >= 4 && mentions(message, lead));
      return named ? undefined : `it reaches ${target.value}, a host your message doesn't name`;
    }
  }
}

const KIND_ALIASES: Record<string, TargetKind | "repo"> = {
  pr: "pr", "pull request": "pr", "pull-request": "pr", pullrequest: "pr", mr: "pr",
  branch: "branch", ref: "branch",
  tag: "tag", release: "tag", version: "tag",
  package: "package", crate: "package", npm: "package",
  host: "host", server: "host", url: "host", domain: "host",
  repo: "repo", repository: "repo",
};

/** The model's target names one the command really acts on. */
function sameTarget(claim: { kind: string; value: string }, target: Target, origin: string | undefined): boolean {
  const kind = KIND_ALIASES[claim.kind.trim().toLowerCase()];
  const value = claim.value.trim().toLowerCase().replace(/^refs\/(heads|tags)\//, "").replace(/^origin\//, "");
  if (!kind || !value) return false;
  if (kind === "repo") return [target.repo, origin].some((repo) => repo !== undefined && (repoName(value) ?? value) === repo);
  if (target.kind === "pr") {
    if (kind === "pr") {
      if (target.action === "create") return [target.base, target.head, "new", target.value].some((name) => name !== undefined && value.replace(/^#/, "").includes(name.toLowerCase()));
      return value.replace(/\D+/g, "") === target.value || value === target.value.toLowerCase();
    }
    return kind === "branch" && target.action === "create" && value === target.base?.toLowerCase();
  }
  if (target.kind === "package") return kind === "package" && [target.value, target.name, target.version].some((name) => name !== undefined && value === name.toLowerCase());
  if (target.kind === "host") return kind === "host" && (value === target.value || value.replace(/^[a-z]+:\/\//, "").split(/[/:]/)[0] === target.value);
  return kind === target.kind && value === target.value.toLowerCase();
}

export interface ExplicitResult {
  ok: boolean;
  /** Why it isn't, for the dialog and the audit. */
  reason?: string;
  quote?: string;
  /** Where the quote was: the plan's instruction or a typed message. */
  quoteSource?: "typed" | "plan";
  targets: string[];
  /** Which checks passed: `quote` (the owner's words), `target` (the command's and the owner's), `current` (not taken back since). */
  passed: ("quote" | "target" | "current")[];
}

/**
 * Whether the owner's own words ask for this line: the model's quote is
 * theirs, names what the line acts on, and nothing they wrote since takes
 * it back.
 */
export function checkExplicit(claim: ExplicitClaim, read: LineTargets, owner: OwnerWords | undefined, entries: readonly Entry[], facts: ExplicitFacts): ExplicitResult {
  const targets = "targets" in read ? read.targets : [];
  const result = (passed: ExplicitResult["passed"], reason?: string, extra: Partial<ExplicitResult> = {}): ExplicitResult => ({
    ok: reason === undefined,
    ...(reason ? { reason } : {}),
    ...(claim.quote ? { quote: claim.quote } : {}),
    targets: targets.map(describeTarget),
    passed,
    ...extra,
  });
  if ("unknown" in read) return result([], `${read.unknown}, so Cypher can't check it against your words`);
  if (!owner) return result([], "Cypher didn't pass your own words to this check (an older Cypher, or a run Cypher didn't start)");
  const quote = claim.quote?.trim();
  if (!quote) return result([], "the reviewing model quoted none of your words");
  const wanted = normalize(quote);
  if (VAGUE.has(wanted)) return result([], `"${quote}" doesn't say what to do`);
  if (weight(wanted) < MIN_QUOTE_WEIGHT) return result([], `"${quote}" is too short to ask for this`);
  let at = -1;
  owner.messages.forEach((message, index) => {
    if (normalize(message.text).includes(wanted)) at = index;
  });
  if (at < 0) {
    const elsewhere = userTexts(entries).some((text) => normalize(text).includes(wanted));
    return result([], elsewhere ? `"${quote}" is in the conversation but not in your own words (an agent's brief or steering, or a translation)` : `"${quote}" isn't in your messages`);
  }
  const message = owner.messages[at];
  const source = { quoteSource: message.source };
  // The model's target must be one the line acts on.
  const origin = targets.find((target) => target.dir)?.dir;
  const originRepo = origin ? facts.originRepo(origin) : undefined;
  const claimed = claim.targets ?? [];
  if (claimed.length === 0) return result(["quote"], "the reviewing model named no target", source);
  const strange = claimed.find((item) => !targets.some((target) => sameTarget(item, target, originRepo)));
  if (strange) return result(["quote"], `the reviewing model named ${strange.kind} ${strange.value}, but the command acts on ${targets.map(describeTarget).join(", ")}`, source);
  // Every target must be what the owner's message asks for.
  const opened = openedPullRequests(entries);
  for (const target of targets) {
    const why = uncovered(target, message.text, opened, facts);
    if (why) return result(["quote"], why, source);
    // "open a PR, don't merge it" asks for no merge, though it says the word.
    if (forbids(message.text, target)) return result(["quote"], `your message says not to (${describeTarget(target)})`, source);
  }
  // Nothing since takes it back or moves on.
  for (const later of owner.messages.slice(at + 1)) {
    const text = later.text.trim();
    const short = text.length > 140 ? `${text.slice(0, 140)}…` : text;
    if (retracts(text, targets)) return result(["quote", "target"], `you wrote later: "${short}"`, source);
    const carriesOn = (text.length <= 120 && (CONTINUATION.test(text.normalize("NFKC").trim()) || CONTINUES.test(text))) || targets.some((target) => mentions(text, target.value));
    if (!carriesOn) return result(["quote", "target"], `you've written since ("${short}"), so that request may be about earlier work`, source);
  }
  return result(["quote", "target", "current"], undefined, source);
}
