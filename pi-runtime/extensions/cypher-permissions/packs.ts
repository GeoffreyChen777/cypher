/**
 * Catalog packs: what the risk classifier (risk.ts) knows about programs,
 * one section per ecosystem. Data, not logic: a new stack needs entries
 * here, and a command no entry names is "local" (the reviewing model looks
 * at it, the user approves the first one).
 *
 * - `INSPECT` (tier R0): commands that only read state the sandbox cannot
 *   reach (other processes, credentials, sockets) and change nothing. They
 *   run outside the sandbox without asking. Every word must be known.
 * - `DANGER` (tier R2): commands that are hard to undo, reach other people
 *   or shared systems, persist beyond the task, or expose secrets. Always a
 *   human, never a rule from a dialog. A word known only when the line runs
 *   counts as whatever an entry wants (the worst case).
 *
 * Entries name a program by its file name and the words after its options
 * (`sub`: `gh pr merge` is `{ program: "gh", sub: ["pr", "merge"] }`). In
 * `sub`, `a|b` is either word and `*` globs within a word. For DANGER the
 * first word must come first and the rest in order anywhere after it, so
 * options and arguments in between do not hide a verb.
 */

import { basename } from "node:path";

import { globalValueOptions } from "./commands.ts";
import { UNKNOWN } from "./shell.ts";

/** One simple command as the entries read it. */
export interface CommandView {
  /** The program's file name. */
  program: string;
  words: readonly string[];
  /** Its words after the program. */
  args: readonly string[];
  /** Words that are not options, nor the values of the CLI's global options. */
  positionals: readonly string[];
}

/** Facts an entry may need about the machine (core.ts provides them). */
export interface PackFacts {
  /** The kube context a command uses: its `--context`, else the current one. */
  kubeContext(args: readonly string[]): string | undefined;
  /** The program resolves inside a place the agent can write (a virtualenv, `node_modules/.bin`). */
  programIsLocal(words: readonly string[]): boolean;
  /** A tool of that name is installed in the project (`node_modules/.bin`, `.venv/bin`). */
  projectTool(name: string): boolean;
}

export interface InspectEntry {
  pack: string;
  program: string | readonly string[];
  sub?: readonly string[];
  /**
   * The sandbox cannot run it (credentials, other processes, a service's
   * socket): a line of inspections runs outside on its own when one needs
   * it. The rest run outside only when the agent escalates them anyway.
   */
  outside?: boolean;
  /** Extra conditions on its words; true when it only reads. */
  check?: (view: CommandView) => boolean;
}

export interface DangerEntry {
  pack: string;
  program: string | readonly string[];
  sub?: readonly string[];
  /** What makes it dangerous, for the dialog: "publishes a package". */
  kind: string;
  /** Extra conditions; true (or a more precise kind) when it is dangerous. */
  when?: (view: CommandView, facts: PackFacts) => boolean | string;
}

export function viewOf(words: readonly string[]): CommandView {
  const program = basename(words[0] ?? "");
  const args = words.slice(1);
  const values = globalValueOptions(program);
  const positionals: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word !== UNKNOWN && word.startsWith("-") && word !== "-") {
      if (positionals.length === 0 && values.includes(word)) i++;
      continue;
    }
    positionals.push(word);
  }
  return { program, words, args, positionals };
}

function wordIs(spec: string, word: string, worst: boolean): boolean {
  if (word === UNKNOWN) return worst;
  return spec.split("|").some((alternative) => {
    if (!alternative.includes("*")) return alternative === word;
    const source = alternative.replace(/[.+^${}()|[\]\\?]/g, "\\$&").replace(/\*/g, ".*");
    return new RegExp(`^${source}$`).test(word);
  });
}

function programIs(entry: { program: string | readonly string[] }, program: string): boolean {
  return typeof entry.program === "string" ? entry.program === program : entry.program.includes(program);
}

/** An inspection entry matches: the same leading words, all known. */
export function inspectMatches(entry: InspectEntry, view: CommandView): boolean {
  if (!programIs(entry, view.program) || view.words.includes(UNKNOWN)) return false;
  const sub = entry.sub ?? [];
  if (sub.some((spec, index) => view.positionals[index] === undefined || !wordIs(spec, view.positionals[index], false))) return false;
  return entry.check ? entry.check(view) : true;
}

/** A danger entry matches, or could once its unknown words are known: the kind, else undefined. */
export function dangerMatches(entry: DangerEntry, view: CommandView, facts: PackFacts): string | undefined {
  if (!programIs(entry, view.program)) return undefined;
  const sub = entry.sub ?? [];
  if (sub.length > 0) {
    const [first, ...rest] = sub;
    const positionals = view.positionals;
    if (positionals.length === 0 || !wordIs(first, positionals[0], true)) return undefined;
    let at = 1;
    for (const spec of rest) {
      while (at < positionals.length && !wordIs(spec, positionals[at], true)) at++;
      if (at >= positionals.length) return undefined;
      at++;
    }
  }
  if (!entry.when) return entry.kind;
  const verdict = entry.when(view, facts);
  return verdict === true ? entry.kind : typeof verdict === "string" ? verdict : undefined;
}

// --- Helpers for conditions -----------------------------------------------------

const has = (view: CommandView, ...flags: string[]) =>
  view.args.some((word) => word === UNKNOWN || flags.some((flag) => word === flag || (flag.startsWith("--") && word.startsWith(`${flag}=`))));

/** A short option given alone or in a bundle (`-f`, `-rf`). */
const hasShort = (view: CommandView, letter: string) => view.args.some((word) => word === UNKNOWN || (/^-[A-Za-z]+$/.test(word) && word.includes(letter)));

const LOCAL_KUBE = /^(kind-.*|minikube|docker-desktop|docker-for-desktop|rancher-desktop|orbstack|k3d-.*|colima.*|microk8s)$/;

/** A cluster that is not on this machine, or not known. */
const remoteCluster = (view: CommandView, facts: PackFacts) => {
  const context = facts.kubeContext(view.args);
  return context === undefined || !LOCAL_KUBE.test(context);
};

const notLocalHost = (url: string) => !/^(https?:\/\/)?(localhost|127\.\d+\.\d+\.\d+|\[::1\]|0\.0\.0\.0)([:/]|$)/.test(url);

// --- Inspection (R0) ---------------------------------------------------------------

/**
 * `ps` without the options that print each process's environment: on
 * macOS `e`, `E` and `-e`; on Linux the BSD-style `e` (`ps aux e`, `ps eww`),
 * while `-e` there only means every process. The value of an option that
 * takes one (`-o etime`) is not an option.
 */
function psReadsNoEnvironment(view: CommandView): boolean {
  const linux = process.platform === "linux";
  for (let i = 0; i < view.args.length; i++) {
    const word = view.args[i];
    if (/^-[oOpgGtuUMq]$/.test(word)) {
      i++;
      continue;
    }
    if (/^-?[A-Za-z]+$/.test(word) && /[eE]/.test(word)) {
      if (linux && word.startsWith("-") && !/E/.test(word) && !/[oOpgGtuUMq][A-Za-z]*e/.test(word.slice(1))) continue;
      return false;
    }
  }
  return true;
}

/** Options that point a CLI at a config or server the line names: the config can run a program (a kubeconfig's exec plugin). */
const refuses =
  (...options: string[]) =>
  (view: CommandView) =>
    !view.args.some((word) => options.some((option) => word === option || word.startsWith(`${option}=`)));

export const INSPECT: readonly InspectEntry[] = [
  // core: processes, files by name, the machine.
  { pack: "core", program: "ps", check: psReadsNoEnvironment },
  // Not -l or -a: they print command lines.
  { pack: "core", program: "pgrep", check: (view) => !view.args.some((word) => /^-[A-Za-z]*[la]/.test(word) || word === "--list-name" || word === "--list-full") },
  { outside: true, pack: "core", program: "lsof" },
  { pack: "core", program: ["ls", "stat", "readlink", "realpath", "du", "df", "which", "whoami", "id", "uname", "hostname", "sw_vers", "uptime", "date", "basename", "dirname"] },
  { outside: true, pack: "core", program: "launchctl", sub: ["list"] },
  { pack: "core", program: "crontab", check: (view) => view.args.length === 1 && view.args[0] === "-l" },
  { outside: true, pack: "core", program: "systemctl", sub: ["status|list-units|list-timers|is-active|is-enabled|is-failed"], check: refuses("-H", "--host", "-M", "--machine") },
  // git hosting: read-only verbs. Not `gh api`, which can write.
  { outside: true, pack: "git hosting", program: "gh", sub: ["run", "list|view|watch"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["pr", "list|view|checks|status|diff"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["issue", "list|view|status"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["release", "list|view"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["workflow", "list|view"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["repo", "view|list"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["search"] },
  { outside: true, pack: "git hosting", program: "gh", sub: ["auth", "status"], check: (view) => !view.args.includes("--show-token") && !view.args.includes("-t") },
  { outside: true, pack: "git hosting", program: "glab", sub: ["mr|issue|release|ci|pipeline|repo", "list|view|status|get"] },
  // git: these run in the sandbox too; outside they also reach remotes. No
  // `-c`, `--config-env`, `--exec-path`, `--git-dir` or `--upload-pack`:
  // each runs a program the line names (core.ts also checks the repository).
  {
    pack: "git",
    program: "git",
    sub: ["log|status|diff|show|rev-parse|cat-file|rev-list|describe|shortlog|blame|ls-files|ls-tree|merge-base|for-each-ref|name-rev|count-objects|show-ref|show-branch|whatchanged"],
    check: (view) => !view.args.some((word) => /^(-c|--config-env|--exec-path|--git-dir|--work-tree|--upload-pack|-u|--ext-diff|--textconv|--output|-o)(=|$)/.test(word) || /^--output=/.test(word)),
  },
  { pack: "git", program: "git", sub: ["branch|tag"], check: (view) => view.positionals.length <= 1 && view.args.every((word) => /^(-l|--list|-a|--all|-r|--remotes|-v|-vv|--verbose|--show-current|--contains|--merged|--no-merged|--sort=.*|--format=.*|-n\d*|--points-at)$/.test(word) || !word.startsWith("-")) && (view.positionals.length === 1 || view.args.some((word) => /^(-l|--list|--show-current|-a|-r|-v|-vv)$/.test(word)) || view.args.length === 1) },
  // A remote may need the keychain's credentials.
  { outside: true, pack: "git", program: "git", sub: ["fetch|ls-remote"], check: (view) => !view.args.some((word) => /^(-c|--config-env|--exec-path|--git-dir|--work-tree|--upload-pack|--receive-pack)(=|$)/.test(word)) },
  { pack: "git", program: "git", sub: ["remote"], check: (view) => view.positionals.length === 1 && view.args.every((word) => word === "remote" || word === "-v" || word === "--verbose") },
  { pack: "git", program: "git", sub: ["worktree|stash", "list"] },
  { pack: "git", program: "git", sub: ["reflog"], check: (view) => view.positionals.length === 1 || view.positionals[1] === "show" },
  // No build tools: each reads the project's configuration, which can name
  // a program it runs (a rustc, a toolexec, a yarnPath, a Maven extension, a
  // Terraform provider), and they all run in the sandbox anyway.
  // Apple and Android.
  { outside: true, pack: "apple", program: "xcrun", sub: ["simctl", "list"] },
  { outside: true, pack: "android", program: "adb", sub: ["devices|version"] },
  // Containers: not `docker inspect`, which prints containers' environments.
  { outside: true, pack: "containers", program: ["docker", "podman"], sub: ["ps|images|version|info|logs|stats|top|port|history"], check: refuses("-H", "--host", "--config", "-c", "--context") },
  { outside: true, pack: "containers", program: ["docker", "podman"], sub: ["system", "df"] },
  { outside: true, pack: "containers", program: ["docker", "podman"], sub: ["context|volume|network|image|container", "ls|list"] },
  { outside: true, pack: "containers", program: ["docker", "podman"], sub: ["compose", "ps|ls|logs|images|top"], check: refuses("-H", "--host", "--config", "--context", "--env-file") },
  { outside: true, pack: "containers", program: "colima", sub: ["list|status|version"] },
  // Kubernetes and infrastructure: reads, never secrets.
  { outside: true, pack: "kubernetes", program: "kubectl", sub: ["get|describe|logs|top|explain|api-resources|api-versions|version|cluster-info"], check: (view) => !view.positionals.some((word) => /^(secrets?|secret\/.*|secrets\/.*)$/.test(word)) && refuses("--kubeconfig", "--server", "-s", "--token", "--as", "--user", "--cluster")(view) },
  { outside: true, pack: "kubernetes", program: "kubectl", sub: ["config", "get-contexts|current-context"], check: refuses("--kubeconfig") },
  { outside: true, pack: "kubernetes", program: "helm", sub: ["list|ls|status|history|version"], check: refuses("--kubeconfig", "--registry-config", "--repository-config", "--repository-cache", "--kube-apiserver", "--kube-token") },
  // Cloud: describe and list verbs, not get-* (several read secrets).
  { outside: true, pack: "cloud", program: "aws", sub: ["*", "describe-*|list-*"], check: (view) => !/^(secretsmanager|ssm|lambda|ecs|apprunner|elasticbeanstalk|amplify|codebuild)$/.test(view.positionals[0] ?? "") && refuses("--endpoint-url", "--cli-input-json", "--cli-input-yaml", "--ca-bundle")(view) },
  { outside: true, pack: "cloud", program: "aws", sub: ["sts", "get-caller-identity"], check: refuses("--endpoint-url", "--ca-bundle") },
  { outside: true, pack: "cloud", program: "aws", sub: ["s3", "ls"], check: refuses("--endpoint-url", "--ca-bundle") },
  { outside: true, pack: "cloud", program: "gcloud", check: (view) => /^(list|describe)$/.test(view.positionals.at(-1) ?? "") && !view.positionals.includes("secrets") },
  { outside: true, pack: "cloud", program: "gcloud", sub: ["config", "list"] },
];

// --- Danger (R2) -----------------------------------------------------------------

const PUBLISH = "publishes a package";
const DEPLOY = "deploys or changes a remote service";
const INSTALL = "installs software for the whole machine";
const SECRET = "reads a secret";
const SEND = "sends a message or data to other people";

/** Variable names that hold secrets. */
const SECRET_NAME = /TOKEN|SECRET|PASSW|KEY|CREDENTIAL|AUTH|COOKIE|SESSION|PRIVATE/i;
const SYSTEM = "changes how this machine runs";

/** `curl`/`wget` sending a body or a non-GET method to a host other than this machine. */
function sendsData(view: CommandView): boolean | string {
  const urls = view.positionals.filter(
    (word) => word === UNKNOWN || /:\/\//.test(word) || /^(localhost|\d+\.\d+\.\d+\.\d+|\[::1\])(:\d+)?(\/|$)/.test(word) || /^[\w-]+(\.[\w-]+)+(:\d+)?(\/|$)/.test(word),
  );
  if (urls.length > 0 && urls.every((url) => url !== UNKNOWN && !notLocalHost(url))) return false;
  if (view.program === "curl") {
    for (let i = 0; i < view.args.length; i++) {
      const word = view.args[i];
      if (word === UNKNOWN) return true;
      if (/^(-d|--data.*|-F|--form.*|-T|--upload-file|--json)(=|$)/.test(word) || /^-[A-Za-z]*[dFT]$/.test(word) && word.length > 2) return true;
      // `-X POST`, `-sX POST`, `-XPOST`, `--request=POST`.
      const method = /^--request=(.*)$/.exec(word)?.[1] ?? (/^-[A-Za-z]*X$/.test(word) || word === "--request" ? (view.args[i + 1] ?? UNKNOWN) : /^-[A-Za-z]*X(.+)$/.exec(word)?.[1]);
      if (method !== undefined && (method === UNKNOWN || !/^(GET|HEAD|OPTIONS)$/i.test(method))) return true;
    }
    return false;
  }
  return view.args.some((word) => word === UNKNOWN || /^--(post-data|post-file|body-data|body-file|method)(=|$)/.test(word));
}

export const DANGER: readonly DangerEntry[] = [
  // Persistence and system.
  { pack: "core", program: ["sudo", "doas", "su", "pkexec"], kind: "runs as the administrator" },
  { pack: "core", program: "launchctl", sub: ["load|bootstrap|enable|submit|unload|bootout|disable|remove"], kind: SYSTEM },
  { pack: "core", program: "systemctl", sub: ["enable|disable|mask|unmask|daemon-reload|edit|set-default|reboot|poweroff|halt|suspend"], kind: SYSTEM },
  { pack: "core", program: "crontab", kind: "changes scheduled jobs", when: (view) => !(view.args.length === 1 && view.args[0] === "-l") },
  { pack: "core", program: "defaults", sub: ["write|delete|import|rename"], kind: SYSTEM },
  { pack: "core", program: ["chsh", "dscl", "scutil", "networksetup", "pmset", "spctl", "csrutil", "nvram", "tccutil", "systemsetup", "kextload", "kmutil", "mkfs", "fdisk", "parted", "shutdown", "reboot", "halt"], kind: SYSTEM, when: (view) => view.program !== "scutil" || has(view, "--set") },
  { pack: "core", program: "diskutil", sub: ["erase*|partition*|reformat|zeroDisk|randomDisk|secureErase|apfs"], kind: "erases or changes a disk" },
  { pack: "core", program: "dd", kind: "writes raw data", when: (view) => view.args.some((word) => word === UNKNOWN || word.startsWith("of=")) },
  // Secrets.
  { pack: "core", program: "security", sub: ["find-generic-password|find-internet-password|dump-keychain|export|unlock-keychain|find-certificate|add-generic-password|add-internet-password|delete-*|set-*"], kind: SECRET, when: (view) => view.positionals[0] !== "find-certificate" || has(view, "-p") },
  { pack: "core", program: ["printenv"], kind: "prints the environment, which holds secrets", when: (view) => view.positionals.length === 0 || view.positionals.some((word) => word === UNKNOWN || SECRET_NAME.test(word)) },
  { pack: "core", program: ["env"], kind: "prints the environment, which holds secrets", when: (view) => view.positionals.length === 0 || view.positionals.every((word) => /^[A-Za-z_]\w*=/.test(word)) },
  { pack: "core", program: ["op"], sub: ["read|item|document|inject|run|signin|account"], kind: SECRET },
  { pack: "core", program: ["pass", "gopass"], kind: SECRET, when: (view) => !/^(ls|list|find|search)$/.test(view.positionals[0] ?? "ls") || view.positionals.length > 1 && view.positionals[0] !== "find" },
  { pack: "core", program: "vault", sub: ["read|kv|write|delete|token|login|secrets|policy|auth"], kind: SECRET },
  { pack: "core", program: ["gpg", "gpg2"], kind: SECRET, when: (view) => view.args.some((word) => word === UNKNOWN || /^--(export-secret-keys|export-secret-subkeys|decrypt|export-ownertrust|delete-secret-keys|delete-secret-and-public-keys)$/.test(word) || /^-[A-Za-z]*d[A-Za-z]*$/.test(word)) },
  { pack: "core", program: "sops", kind: SECRET, when: (view) => view.args.some((word) => word === UNKNOWN || /^(-d|--decrypt|exec-env|exec-file|decrypt|edit|-i|--in-place|set|unset)$/.test(word)) || view.positionals.length === 1 },
  { pack: "core", program: ["bw", "rbw"], sub: ["get|list|export|unlock|login|serve|send|sync"], kind: SECRET },
  { pack: "core", program: "doppler", sub: ["secrets|run|configs"], kind: SECRET },
  { pack: "core", program: "infisical", sub: ["secrets|run|export|login"], kind: SECRET },
  { pack: "core", program: ["chamber", "aws-vault", "envchain", "berglas", "teller"], kind: SECRET },
  { pack: "core", program: "keyring", sub: ["get|set|del"], kind: SECRET },
  { pack: "git", program: "git", sub: ["credential", "fill|approve|reject"], kind: SECRET },
  // Send.
  { pack: "core", program: ["mail", "mailx", "sendmail", "mutt", "msmtp", "neomutt"], kind: SEND },
  { pack: "core", program: ["slack", "slackcat", "slack-cli", "telegram-send", "signal-cli", "matrix-commander", "discord-send", "twilio", "ntfy", "apprise", "pushover"], kind: SEND, when: (view) => !view.args.every((word) => /^(-h|--help|--version|help)$/.test(word)) || view.args.length === 0 },
  { pack: "core", program: "osascript", kind: "controls apps on this machine (it can send mail and messages)" },
  { pack: "core", program: ["open", "xdg-open"], kind: "opens a link, which can send data to a site", when: (view) => view.positionals.some((word) => word === UNKNOWN || (/^[a-z][\w+.-]*:/i.test(word) && !/^file:/i.test(word) && !/^(https?:\/\/)?(localhost|127\.\d+\.\d+\.\d+|\[::1\])([:/]|$)/i.test(word))) },
  { pack: "core", program: ["curl", "wget"], kind: "sends data to a server", when: sendsData },
  // scp and rsync are decided in risk.ts: by the machines and local files they copy between.
  // Git: publishing and destroying are decided in risk.ts (they need the repository).
  // `git config` is decided in risk.ts: by the key it sets.
  { pack: "git", program: "git", sub: ["remote", "add|set-url|rename|remove|rm|set-head|set-branches"], kind: "changes the repository's remotes" },
  { pack: "git", program: "git", sub: ["stash", "drop|clear"], kind: "drops stashed work, which every worktree of the repository shares" },
  { pack: "git", program: "git", sub: ["send-email|request-pull|svn|p4"], kind: SEND },
  { pack: "git hosting", program: "gh", sub: ["release", "create|delete|edit|upload|delete-asset"], kind: "publishes a release" },
  { pack: "git hosting", program: "gh", sub: ["pr", "create|merge|comment|review|close|reopen|edit|ready|lock|unlock"], kind: "changes a pull request other people see" },
  { pack: "git hosting", program: "gh", sub: ["issue", "create|comment|close|reopen|edit|delete|transfer|lock|unlock|pin|unpin"], kind: "changes an issue other people see" },
  { pack: "git hosting", program: "gh", sub: ["repo", "create|delete|edit|rename|archive|unarchive|fork|sync|deploy-key|set-default"], kind: "changes a repository on GitHub" },
  { pack: "git hosting", program: "gh", sub: ["secret|variable", "set|delete|remove"], kind: "changes a repository's secrets or variables" },
  { pack: "git hosting", program: "gh", sub: ["gist", "create|edit|delete|clone"], kind: "publishes a gist", when: (view) => view.positionals[1] !== "clone" },
  { pack: "git hosting", program: "gh", sub: ["ssh-key|gpg-key", "add|delete"], kind: "changes the account's keys" },
  { pack: "git hosting", program: "gh", sub: ["auth", "login|logout|refresh|token|setup-git|switch"], kind: SECRET },
  { pack: "git hosting", program: "gh", sub: ["auth", "status"], kind: SECRET, when: (view) => view.args.some((word) => word === UNKNOWN || word === "--show-token" || /^-[A-Za-z]*t/.test(word)) },
  { pack: "git hosting", program: "gh", sub: ["api"], kind: "writes through the GitHub API", when: (view) => has(view, "-f", "-F", "--field", "--raw-field", "--input") || view.args.some((word, i) => (word === "-X" || word === "--method") && !/^GET$/i.test(view.args[i + 1] ?? "")) || view.args.some((word) => /^(-X|--method=)(?!GET$)\S+/i.test(word) && word !== "-X") },
  { pack: "git hosting", program: "gh", sub: ["workflow", "enable|disable"], kind: DEPLOY },
  { pack: "git hosting", program: "glab", sub: ["mr|issue", "create|merge|note|close|reopen|update|delete|approve"], kind: "changes a merge request or issue other people see" },
  { pack: "git hosting", program: "glab", sub: ["release|variable|repo|ssh-key", "create|delete|set|update|upload|archive|transfer|add"], kind: "changes a project on GitLab" },
  { pack: "git hosting", program: "glab", sub: ["auth", "login|logout"], kind: SECRET },
  // JavaScript.
  { pack: "javascript", program: ["npm", "pnpm", "yarn", "bun"], sub: ["publish|unpublish|deprecate|owner|dist-tag|access|team|token|adduser|login|logout"], kind: PUBLISH, when: (view) => view.positionals[0] !== "dist-tag" || view.positionals[1] !== "ls" },
  { pack: "javascript", program: ["npm", "pnpm", "bun"], sub: ["install|i|add|uninstall|remove|rm|update|up|link|unlink"], kind: INSTALL, when: (view) => has(view, "-g", "--global", "--location=global") },
  { pack: "javascript", program: "yarn", sub: ["global"], kind: INSTALL },
  { pack: "javascript", program: ["npx", "bunx", "pnpx"], kind: "downloads and runs a package", when: (view, facts) => !facts.projectTool(view.positionals[0] ?? UNKNOWN) },
  { pack: "javascript", program: ["npm"], sub: ["exec|x"], kind: "downloads and runs a package", when: (view, facts) => !facts.projectTool(view.positionals[1] ?? UNKNOWN) },
  { pack: "javascript", program: ["pnpm", "yarn"], sub: ["dlx"], kind: "downloads and runs a package" },
  // Python.
  { pack: "python", program: ["pip", "pip3"], sub: ["install|uninstall"], kind: "changes the system's Python packages", when: (view, facts) => has(view, "--user", "--break-system-packages", "--target", "--prefix", "--root") || !facts.programIsLocal(view.words) },
  { pack: "python", program: ["twine", "flit", "hatch"], sub: ["upload|publish"], kind: PUBLISH },
  { pack: "python", program: ["uv", "poetry", "pdm"], sub: ["publish"], kind: PUBLISH },
  { pack: "python", program: "uv", sub: ["tool", "install|uninstall|upgrade"], kind: INSTALL },
  { pack: "python", program: "uv", sub: ["pip", "install|uninstall|sync"], kind: INSTALL, when: (view) => has(view, "--system", "--break-system-packages") },
  { pack: "python", program: "pipx", sub: ["install|uninstall|upgrade|inject|run|reinstall-all|uninstall-all"], kind: INSTALL },
  { pack: "python", program: "uvx", kind: "downloads and runs a package", when: (view, facts) => !facts.projectTool(view.positionals[0] ?? UNKNOWN) },
  // Ruby.
  { pack: "ruby", program: "gem", sub: ["install|uninstall|update|push|yank|owner|signin|signout"], kind: INSTALL, when: (view) => (/^(push|yank|owner|signin|signout)$/.test(view.positionals[0] ?? "") ? PUBLISH : true) },
  // Rust, Go, JVM, .NET.
  { pack: "rust", program: "cargo", sub: ["publish|yank|owner|login|logout"], kind: PUBLISH },
  { pack: "rust", program: "cargo", sub: ["install|uninstall"], kind: INSTALL },
  { pack: "rust", program: "rustup", sub: ["self|set|default|toolchain"], kind: INSTALL, when: (view) => view.positionals[1] !== "list" },
  { pack: "go", program: "go", sub: ["install"], kind: INSTALL },
  { pack: "jvm", program: ["mvn", "mvnw", "./mvnw"], kind: PUBLISH, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(deploy|release:.*|.*:deploy)$/.test(word)) },
  { pack: "jvm", program: ["gradle", "gradlew", "./gradlew"], kind: PUBLISH, when: (view) => view.positionals.some((word) => word === UNKNOWN || (/(^|:)publish/.test(word) && !/MavenLocal$/.test(word)) || /(^|:)(release|uploadArchives)$/.test(word)) },
  { pack: "jvm", program: "sbt", kind: PUBLISH, when: (view) => view.positionals.some((word) => word === UNKNOWN || /publish(Signed)?$|release/.test(word) && !/publishLocal/.test(word)) },
  { pack: "dotnet", program: "dotnet", sub: ["nuget", "push|delete"], kind: PUBLISH },
  { pack: "dotnet", program: "dotnet", sub: ["tool", "install|uninstall|update"], kind: INSTALL, when: (view) => has(view, "-g", "--global") },
  // Apple and Android.
  { pack: "apple", program: "xcrun", sub: ["altool|notarytool|iTMSTransporter"], kind: "uploads to Apple", when: (view) => !/^(history|info|log|list)$/.test(view.positionals[1] ?? "") },
  { pack: "apple", program: "fastlane", kind: "uploads or releases an app", when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(deliver|pilot|upload_to_.*|release|beta|appstore|supply|match|cert|sigh|pem)$/.test(word)) },
  { pack: "android", program: "adb", sub: ["uninstall|shell"], kind: "changes a connected device", when: (view) => view.positionals[0] === "uninstall" || view.positionals.some((word) => word === UNKNOWN || /^(rm|pm)$/.test(word)) },
  // Containers.
  { pack: "containers", program: ["docker", "podman"], sub: ["push|login|logout"], kind: PUBLISH, when: (view) => (view.positionals[0] === "push" ? true : SECRET) },
  { pack: "containers", program: ["docker", "podman"], sub: ["volume", "rm|prune"], kind: "deletes container volumes and their data" },
  { pack: "containers", program: ["docker", "podman"], sub: ["system", "prune|reset"], kind: "deletes containers, images and volumes" },
  { pack: "containers", program: ["docker", "podman"], sub: ["buildx|build", "*"], kind: PUBLISH, when: (view) => has(view, "--push") },
  { pack: "containers", program: ["docker", "podman"], sub: ["compose", "push|down"], kind: "deletes container volumes and their data", when: (view) => (view.positionals.includes("push") ? PUBLISH : has(view, "-v", "--volumes")) },
  {
    pack: "containers",
    program: ["docker", "podman"],
    sub: ["run|create|exec"],
    kind: "runs a container with access to this machine",
    when: (view) =>
      view.args.some((word, i) => {
        if (word === UNKNOWN) return true;
        if (/^--(privileged|pid=host|ipc=host|userns=host|cap-add.*|device.*|security-opt.*)$/.test(word)) return true;
        const mount = /^(-v|--volume|--mount)$/.test(word) ? view.args[i + 1] : /^(--volume=|--mount=)(.*)$/.exec(word)?.[2];
        if (mount === undefined) return false;
        if (mount === UNKNOWN) return true;
        const source = /(?:^|,)(?:source|src)=([^,]+)/.exec(mount)?.[1] ?? mount.split(":")[0];
        return /^(\/|~\/?|\/Users\/?[^/]*\/?|\/home\/?[^/]*\/?|\/var\/run\/docker\.sock|\/run\/docker\.sock)$/.test(source.replace(/\/$/, "") || "/") || /\.ssh|\.aws|\.config\/gcloud|\.kube|\.gnupg|docker\.sock/.test(source);
      }),
  },
  // Kubernetes and infrastructure. A cluster on this machine is local work.
  { pack: "kubernetes", program: "kubectl", sub: ["apply|create|delete|replace|patch|scale|rollout|edit|exec|cp|drain|cordon|uncordon|taint|label|annotate|set|expose|run|attach|debug|certificate|autoscale"], kind: "changes a Kubernetes cluster", when: remoteCluster },
  { pack: "kubernetes", program: "kubectl", sub: ["get"], kind: SECRET, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(secrets?|secret\/.*|secrets\/.*)$/.test(word)) },
  { pack: "kubernetes", program: "kubectl", sub: ["config", "view"], kind: SECRET, when: (view) => has(view, "--raw", "--flatten") },
  { pack: "kubernetes", program: "helm", sub: ["install|upgrade|uninstall|delete|rollback|push"], kind: "changes a Kubernetes cluster", when: (view, facts) => remoteCluster({ ...view, args: view.args.map((word) => (word === "--kube-context" ? "--context" : word.replace(/^--kube-context=/, "--context="))) }, facts) },
  { pack: "infrastructure", program: ["terraform", "tofu"], sub: ["apply|destroy|import|taint|untaint|state|force-unlock|workspace"], kind: DEPLOY, when: (view) => (view.positionals[0] === "state" ? /^(mv|rm|push|replace-provider)$/.test(view.positionals[1] ?? UNKNOWN) || view.positionals[1] === UNKNOWN : view.positionals[0] === "workspace" ? view.positionals[1] === "delete" || view.positionals[1] === UNKNOWN : true) },
  { pack: "infrastructure", program: "pulumi", sub: ["up|update|destroy|cancel|import|refresh|state"], kind: DEPLOY },
  { pack: "infrastructure", program: ["cdk", "sst", "serverless", "sls"], sub: ["deploy|destroy|remove|bootstrap"], kind: DEPLOY },
  { pack: "infrastructure", program: ["ansible", "ansible-playbook"], kind: DEPLOY, when: (view) => !has(view, "--check", "-C", "--syntax-check", "--list-hosts", "--list-tasks") },
  // Cloud and hosting.
  {
    pack: "cloud",
    program: "aws",
    kind: DEPLOY,
    when: (view) => {
      const [service, verb] = view.positionals;
      if (service === undefined) return false;
      if (service === UNKNOWN || verb === UNKNOWN) return true;
      if (service === "configure") return SECRET;
      if (/^(secretsmanager|ssm|kms)$/.test(service) && /^(get-|decrypt|generate-data-key)/.test(verb ?? "")) return SECRET;
      if (/^(ecr|codeartifact|sts)$/.test(service) && /^(get-login-password|get-authorization-token|get-session-token|assume-role)/.test(verb ?? "")) return SECRET;
      if (service === "s3") return /^(rm|mv|rb|mb|sync|presign|website)$/.test(verb ?? "") || (verb === "cp" && /^s3:\/\//.test(view.positionals.at(-1) ?? ""));
      return /^(create|delete|put|update|modify|terminate|remove|attach|detach|run|start|stop|reboot|reset|restore|revoke|authorize|associate|disassociate|import|register|deregister|tag|untag|invoke|publish|send|set|enable|disable|cancel|execute|copy|replace|accept|reject|purge|rotate|batch-write|batch-delete|deploy|upload)(-|$)/.test(verb ?? "");
    },
  },
  {
    pack: "cloud",
    program: "gcloud",
    kind: DEPLOY,
    when: (view) => {
      if (view.positionals[0] === "config") return false;
      if (view.positionals[0] === "auth") return /^(print-access-token|print-identity-token|application-default)$/.test(view.positionals[1] ?? UNKNOWN) ? SECRET : false;
      if (view.positionals.includes("secrets") && view.positionals.includes("access")) return SECRET;
      return view.positionals.some((word) => word === UNKNOWN || /^(create|delete|deploy|update|set-.*|add-.*|remove-.*|patch|import|reset|resize|start|stop|submit|apply|replace|rollback|cancel|enable|disable|ssh|scp)$/.test(word));
    },
  },
  { pack: "cloud", program: "az", kind: DEPLOY, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(create|delete|deploy|update|set|add|remove|start|stop|restart|import|purge|assign|invoke|run-command)$/.test(word)) },
  { pack: "cloud", program: "doctl", kind: DEPLOY, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(create|delete|update|deploy|apply|rm|destroy)$/.test(word)) },
  {
    pack: "hosting",
    program: "vercel",
    kind: DEPLOY,
    when: (view) => {
      // `vercel` alone deploys.
      const [first = "deploy", second] = view.positionals;
      if (first === "env") return !/^(ls|pull)$/.test(second ?? UNKNOWN);
      return !/^(dev|build|pull|link|ls|list|logs|inspect|whoami|help|login|switch)$/.test(first);
    },
  },
  { pack: "hosting", program: "netlify", sub: ["deploy|sites:delete|env:set|env:unset|functions:invoke|api"], kind: DEPLOY },
  { pack: "hosting", program: ["fly", "flyctl"], sub: ["deploy|destroy|secrets|scale|launch|apps|machine|machines|volumes|ips|certs|postgres|redis|releases"], kind: DEPLOY, when: (view) => !/^(list|ls|show|status|logs)$/.test(view.positionals[1] ?? "deploy") },
  { pack: "hosting", program: "wrangler", sub: ["deploy|publish|delete|secret|kv:*|kv|r2|d1|rollback|versions|pages|queues"], kind: DEPLOY, when: (view) => !view.positionals.some((word) => /^(list|ls|get|info|dev|tail)$/.test(word)) && !(view.positionals[0] === "d1" && !has(view, "--remote")) },
  { pack: "hosting", program: "heroku", kind: DEPLOY, when: (view) => !/^(logs|ps|apps:info|releases|info|local|login|whoami|apps|addons|config|config:get|help)$/.test(view.positionals[0] ?? UNKNOWN) },
  { pack: "hosting", program: "firebase", kind: DEPLOY, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(deploy|.*:delete|.*:deploy|.*:set|.*:destroy|.*:clone|.*:rollback)$/.test(word)) },
  { pack: "hosting", program: "supabase", kind: DEPLOY, when: (view) => view.positionals.some((word) => word === UNKNOWN || /^(push|deploy|delete|set|unset|reset)$/.test(word)) && !has(view, "--local") },
  { pack: "hosting", program: "railway", sub: ["up|deploy|delete|down|variables|redeploy"], kind: DEPLOY },
  // Package managers of the machine.
  { pack: "system packages", program: ["brew"], sub: ["install|uninstall|remove|rm|reinstall|upgrade|tap|untap|link|unlink|pin|services|autoremove"], kind: INSTALL, when: (view) => view.positionals[0] !== "services" || !/^(list|info)$/.test(view.positionals[1] ?? UNKNOWN) },
  { pack: "system packages", program: ["port", "apt", "apt-get", "dnf", "yum", "pacman", "zypper", "apk", "snap", "flatpak", "mas"], sub: ["install|remove|uninstall|purge|upgrade|update|selfupdate|autoremove|refresh"], kind: INSTALL },
  { pack: "system packages", program: ["pacman", "yay", "paru"], kind: INSTALL, when: (view) => view.args.some((word) => word === UNKNOWN || /^-[SRU]/.test(word)) },
];
