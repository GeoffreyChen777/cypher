/**
 * Lists closed by their default (final pass, iteration 3; design §7.3.16). Three red teams in a row found new members
 * of open lists (senders, wrappers, credential names). Here each list says the other side, which is finite: the
 * programs known to keep what they are given on this machine (`LOCAL_CONSUMERS`), the programs known not to run their
 * operands (`PLAIN_PROGRAMS`), and the options by which a tool takes its own credential (`OWN_CREDENTIAL`). Anything
 * else is read as possibly sending, possibly running: the floor's readers (core.ts) decide with that default.
 * Secrets are also known by what a file holds (`secretByContent`), not only by its name.
 */

import { closeSync, openSync, readSync, statSync } from "node:fs";

/**
 * Programs known to keep their input on this machine (data): readers, filters, encoders, archivers into a file,
 * editors, the clipboard, the shell's builtins. A secret's contents handed to any other program may leave it.
 * Interpreters are read by their code (code.ts), not here.
 */
export const LOCAL_CONSUMERS: ReadonlySet<string> = new Set([
  // the shell's own
  "echo", "printf", "read", "test", "[", "true", "false", ":", "export", "local", "declare", "typeset", "set", "unset", "eval", "source", ".", "cd", "pushd", "popd", "mapfile", "readarray", "let",
  // readers and filters
  "cat", "tac", "head", "tail", "less", "more", "most", "bat", "grep", "egrep", "fgrep", "rg", "ag", "ack", "awk", "gawk", "mawk", "nawk", "sed", "gsed", "jq", "yq", "xq", "gron", "cut", "tr", "sort", "uniq", "wc", "nl", "paste", "column", "fold", "fmt", "expand", "unexpand", "rev", "strings", "file", "stat", "ls",
  "diff", "cmp", "comm", "colordiff", "delta", "md5", "md5sum", "shasum", "sha1sum", "sha256sum", "sha512sum", "b2sum", "cksum",
  // encoders, archivers into files, copies on this machine
  "base64", "base32", "xxd", "od", "hexdump", "iconv", "envsubst", "gzip", "gunzip", "zcat", "bzip2", "bunzip2", "xz", "unxz", "zstd", "lz4", "tar", "gtar", "bsdtar", "zip", "unzip", "7z", "7za", "split", "tee", "dd", "cp", "mv", "ln", "install", "mkdir", "touch", "chmod", "chown", "age", "sops",
  // editors and the clipboard
  "vi", "vim", "nvim", "nano", "emacs", "ed", "code", "subl", "pbcopy", "pbpaste", "xclip", "xsel", "wl-copy",
  // xargs and parallel hand their input to the program they run: that program is the consumer (flow.ts).
]);

/**
 * Programs known not to run their operands as commands (data): text printers and searchers, message options. Their
 * string operands that look like command lines (a commit message, a search pattern) are text. Programs that run a
 * command somewhere else (a container, a pod, another host: `REMOTE_RUNNERS`) are no wrappers for this machine either.
 */
export const PLAIN_PROGRAMS: ReadonlySet<string> = new Set([
  "echo", "printf", "cat", "less", "more", "man", "info", "tldr", "grep", "egrep", "fgrep", "rg", "ag", "ack", "git", "gh", "glab", "jq", "yq", "sed", "gsed", "awk", "gawk", "mawk",
  "psql", "mysql", "sqlite3", "redis-cli", "mongosh", "mongo", "duckdb", "osascript", "say", "notify-send", "terminal-notifier", "logger", "wall", "write", "tee", "test", "[", "alias",
  "python", "python3", "node", "ruby", "perl", "php", "deno", "bun", "lua", "Rscript", "julia", "swift",
]);
export const REMOTE_RUNNERS: ReadonlySet<string> = new Set(["ssh", "mosh", "docker", "podman", "kubectl", "oc", "multipass", "lxc", "vagrant", "limactl", "colima", "orb", "gcloud", "aws", "az", "fly", "flyctl", "heroku", "railway"]);

/** Whether a command runs its words somewhere else (a host, a container, a pod): `ssh`, `docker exec|run`, `kubectl exec`. */
export function runsElsewhere(words: readonly string[]): boolean {
  const program = (words[0] ?? "").replace(/^.*\//, "");
  if (program === "ssh" || program === "mosh") return true;
  if (!REMOTE_RUNNERS.has(program)) return false;
  return words.slice(1).some((word) => /^(exec|run|attach|ssh|shell|console|debug)$/.test(word));
}

/** Option values that are text, never a command (a message, a title, a body, a search pattern). */
export const TEXT_OPTIONS = /^(-m|--message|-t|--title|-b|--body|--description|-d|--grep|-e|--regexp|--subject|--notes|-F|--file)$/;

/**
 * Options by which a tool takes its own credential or configuration (data, by name): `-i KEY`, `--password X`,
 * `--with-token`, `--kubeconfig F`, `--env-file F`. What reaches them the tool uses to do its own work: the judge's.
 */
export const OWN_CREDENTIAL = /^--?(key|keyfile|key-file|private-key|identity|identity-file|identity_file|cert|cert-key|cacert|ca-file|password|passwd|pass|password-file|password-stdin|with-token|token|token-file|secret|secret-file|credentials|credential|credentials-file|auth|auth-file|kubeconfig|config|env-file|netrc-file|ssh-key|service-account|sa-key)(=.*)?$/i;

/** Options whose value is a file the program writes (what it receives, not what it sends). */
export const OUTPUT_OPTIONS = /^(-o|--output|--output-document|--output-file|--out|--outfile|--output-dir|--dump-header|--cookie-jar|--log-file|--trace|--trace-ascii|--directory-prefix|--save|--dest|--destination|--to|--download|--downloads)(=.*)?$/;

/** A host another machine has: not this one. */
export function remoteHost(host: string): boolean {
  const h = host.toLowerCase().replace(/^\[|\]$/g, "");
  return Boolean(h) && !/^(localhost|127\.\d+\.\d+\.\d+|0\.0\.0\.0|::1|::|host\.docker\.internal)$/.test(h) && !h.endsWith(".local") && !h.startsWith("unix:");
}

/** Options that name the machine a program talks to (by name: `-h HOST`, `--host`, `-H`, `--server`, `-n` for logger). */
const HOST_OPTIONS = /^(-h|-H|--host|--hostname|--server|-S|--broker|-n|--url|--endpoint|--address|--addr)(=.*)?$/;

/** The other machine a command's words name: a host option's value, a URL, `//host/share`, `user@host:path`, `ssh://`. */
export function namedRemote(words: readonly string[], unknown: string): string | undefined {
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (word === unknown) continue;
    const option = HOST_OPTIONS.exec(word);
    if (option) {
      const value = option[2] ? option[2].slice(1) : words[i + 1];
      const host = value && value !== unknown ? value.replace(/^[a-z]+:\/\//i, "").replace(/[:/].*$/, "") : "";
      // A machine's name (a dotted name with a top-level domain, or an IPv4 address), not a count or a size.
      if ((/^[\w.-]+\.[a-z]{2,}$/i.test(host) || /^\d+\.\d+\.\d+\.\d+$/.test(host)) && remoteHost(host)) return host;
      continue;
    }
    const url = /^[a-z][\w+.-]*:\/\/(?:[^@/]*@)?([^/:?#]+)/i.exec(word);
    if (url && remoteHost(url[1])) return url[1];
    const share = /^\/\/([^/]+)\//.exec(word);
    if (share && remoteHost(share[1])) return share[1];
    const scp = /^(?:[\w.-]+@)?([\w.-]+\.[a-z]{2,}|\d+\.\d+\.\d+\.\d+):/i.exec(word);
    if (scp && remoteHost(scp[1])) return scp[1];
  }
  return undefined;
}

/**
 * Markers of secret material in a file's first bytes (data): private keys, cloud and forge tokens, and env-style or
 * config lines that set a credential-named key to a value. By content, so a key under a neutral name is a key.
 */
const CONTENT_MARKERS: readonly RegExp[] = [
  /-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED |PGP )?PRIVATE KEY(?: BLOCK)?-----/,
  /\bAKIA[0-9A-Z]{16}\b/,
  /\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}\b|\bgithub_pat_[A-Za-z0-9_]{40,}\b/,
  /\bglpat-[A-Za-z0-9_-]{20,}\b/,
  /\bxox[abpors]-[A-Za-z0-9-]{10,}\b/,
  /\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{10,}\b/,
  /\bAIza[0-9A-Za-z_-]{35}\b/,
  /\bnpm_[A-Za-z0-9]{36}\b|_authToken\s*=\s*\S+/,
  /\bAGE-SECRET-KEY-1[0-9A-Z]{50,}\b/,
  /"(?:private_key|refresh_token|access_token|client_secret|api_key)"\s*:\s*"[^"]{8,}"/,
  /<password>[^<]{3,}<\/password>/i,
];

/** Lines `NAME=value` / `name: value` / `"name": "value"` whose name says it is a credential (`secretName`). */
function credentialLine(text: string, secretName: (name: string) => boolean): boolean {
  for (const line of text.split("\n").slice(0, 400)) {
    const kv = /^\s*(?:export\s+)?:?["']?([A-Za-z_][\w.-]*)["']?\s*[:=]\s*["']?([^\s"'#][^"'\n]*)/.exec(line);
    if (kv && secretName(kv[1].replace(/[.-]/g, "_").toUpperCase()) && !/^(\$\{?|<|changeme|xxx|your[_-]|example|placeholder|todo)/i.test(kv[2])) return true;
  }
  return false;
}

/** Whether a file holds secret material by its content (`CONTENT_MARKERS`, credential lines); reads at most 16 KB. */
export function secretByContent(path: string, secretName: (name: string) => boolean): boolean {
  let fd: number | undefined;
  try {
    const stat = statSync(path);
    if (!stat.isFile() || stat.size === 0 || stat.size > 4 * 1024 * 1024) return false;
    fd = openSync(path, "r");
    const buffer = Buffer.alloc(16 * 1024);
    const read = readSync(fd, buffer, 0, buffer.length, 0);
    const text = buffer.subarray(0, read).toString("utf8");
    if (text.includes("\u0000")) return /-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(text);
    if (CONTENT_MARKERS.some((marker) => marker.test(text)) || credentialLine(text, secretName)) return true;
    // A derived copy (iteration 4): a file that is one base64 or hex blob is read as what it decodes to.
    const blob = text.replace(/\s+/g, "");
    let decoded: string | undefined;
    if (blob.length >= 16 && /^[A-Za-z0-9+/]+={0,2}$/.test(blob) && blob.length % 4 === 0) decoded = Buffer.from(blob, "base64").toString("utf8");
    else if (blob.length >= 16 && /^(?:[0-9a-fA-F]{2})+$/.test(blob)) decoded = Buffer.from(blob, "hex").toString("utf8");
    return decoded !== undefined && !/[\u0000-\u0008\u000e-\u001f]/.test(decoded) && (CONTENT_MARKERS.some((marker) => marker.test(decoded!)) || credentialLine(decoded, secretName));
  } catch {
    return false;
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/**
 * Programs whose name, as an operand word, starts a command line another program may run (data: the shells, the
 * interpreters, and the programs the floor's tables read). Used with the default below, not as a list of wrappers:
 * any program not in `PLAIN_PROGRAMS` that is given such words may run them.
 */
export const COMMAND_STARTS: ReadonlySet<string> = new Set([
  "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3", "node", "nodejs", "tsx", "ruby", "perl", "php", "deno", "bun", "osascript",
  "rm", "rmdir", "cp", "mv", "dd", "tee", "truncate", "ln", "chmod", "install", "kill", "pkill", "killall", "git", "gh", "glab", "docker", "kubectl", "npm", "pnpm", "yarn", "make",
  "curl", "wget", "http", "https", "httpie", "xh", "nc", "ncat", "netcat", "socat", "ssh", "scp", "sftp", "rsync", "ftp", "telnet", "mail", "sendmail", "aws", "gcloud", "az", "gsutil", "rclone",
]);

/** Words that, inside one operand's text, come before the command it runs (`|curl …` for LESSOPEN, `exec …`). */
export function commandText(text: string): string | undefined {
  const trimmed = text.trim().replace(/^\|\s*/, "").replace(/^(exec|command|nohup)\s+/, "").replace(/\s+%s\s*$/, "").replace(/\s+#\s*$/, "");
  const first = /^([\w./+-]+)\s/.exec(trimmed)?.[1];
  if (!first) return undefined;
  const name = first.replace(/^.*\//, "");
  return COMMAND_STARTS.has(name) ? trimmed : undefined;
}

/**
 * Places that leave the machine by themselves (iteration 4, data): folders a sync client uploads (iCloud Drive,
 * CloudStorage providers, Dropbox, Google Drive, OneDrive, Box) and volumes mounted under /Volumes (a share, a
 * removable disk). A copy of a secret into one is a send.
 */
export const OFF_MACHINE_PLACES: readonly RegExp[] = [
  /^~\/Library\/Mobile Documents\//, /^~\/Library\/CloudStorage\//, /^~\/(?:Dropbox|Google Drive|OneDrive[^/]*|Box|Box Sync|pCloud Drive|MEGA|Sync|Nextcloud|ownCloud)(?:\/|$)/, /^\/Volumes\/[^/]+\//, /^\/(?:mnt|media)\/[^/]+\/[^/]+/,
];

/** The off-machine place a path (`~`-relative or absolute) is in, if any. */
export function offMachinePlace(display: string): string | undefined {
  return OFF_MACHINE_PLACES.some((place) => place.test(display)) ? display : undefined;
}
