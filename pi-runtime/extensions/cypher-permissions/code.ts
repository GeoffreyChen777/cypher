/**
 * Code as data (round 5, design §7.3.1 row 1): what an interpreter runs is read like the shell line around
 * it. `python3 -c`, `node -e`, `ruby -e`, `perl -e`, `osascript -e`, `deno eval`, `bun -e`, `php -r`, a
 * heredoc, here-string or pipe fed to an interpreter, and a script this task wrote and then runs are code
 * the floor's tables apply to: a sensitive source (a secret file it reads, a credential variable, a
 * credential command it runs) together with any network capability in the same code — or in the line
 * around it — is a secret leaving this machine (floor.ts `secret-off-machine`).
 *
 * Three tables, no per-case rules:
 * - `INTERPRETERS`: interpreter → the flags that take inline code, its subcommands, how it reads code
 *   from its input, its scripts' extensions;
 * - `CAPABILITIES`: per language, the names that reach the network, run a program, read a file, read the
 *   environment, find the home folder, or build code at run time (`exec(base64.b64decode(…))`);
 * - the shell's own tables for what code runs through a program (`subprocess.run(["curl", …])` is read
 *   as the shell line `curl …`, risk.ts `NETWORK_SINKS`/`SECRET_SOURCES`).
 *
 * Code the scanner can't read — built or decoded at run time, obfuscated, a binary, a script that isn't
 * there — goes to the judge with the evidence "opaque code" (judge.ts `opaqueCode`, concern
 * `opaque-code`), never as local work.
 */

export type Language = "python" | "javascript" | "ruby" | "perl" | "php" | "applescript" | "lua" | "r" | "tcl" | "shell" | "julia" | "powershell" | "swift" | "awk" | "sed" | "make" | "unread";

export interface Interpreter {
  language: Language;
  /** Program names (basename of the first word). */
  programs: RegExp;
  /** Options whose value is inline code (`-c`, `-e`, `--eval`); `--eval=CODE` and `-cCODE` too. */
  inline: readonly string[];
  /** One-letter flags that, ending a cluster, take inline code (`perl -ne`, `ruby -we`). */
  clusters?: string;
  /** Subcommands: their next word is inline code, or a script (`deno eval`, `deno run`, `bun run`). */
  subcommands?: Readonly<Record<string, "inline" | "script">>;
  /** Options that take a value (so it is no script). */
  values: readonly string[];
  /** Options after which it runs a module or a named program, not a script it reads (`python -m`). */
  modules?: readonly string[];
  /** Options whose value is a module the code loads (`perl -MLWP::Simple`, `ruby -rnet/http`, `node -r`): part of the code read. */
  loads?: readonly string[];
  /** With no script (or `-`), it reads its program from its input. */
  stdin: boolean;
  /** Its first operand is its program (`awk '{…}' f`, `sed 's/…/' f`), unless an inline flag gave one; the rest are data. */
  codeOperand?: boolean;
  /** Options whose value is a script file it runs (`awk -f prog.awk`, `sed -f s.sed`). */
  scripts?: readonly string[];
  /** Options whose value is code that can't be read here (`pwsh -EncodedCommand`). */
  opaqueInline?: readonly string[];
  /**
   * A text filter whose program often carries the shell's variables (`sed "s/x/$V/"`, `awk -v`): a program
   * known only when it runs is no finding of its own (the shell's reading of the line stands).
   */
  filter?: boolean;
  /** Options with an optional value: the next word is theirs only when it matches (`sed -i ''`, `sed -i .bak`). */
  optionalValues?: Readonly<Record<string, RegExp>>;
  /** Its scripts' extensions, for a script run as `./x` without a `#!` line. */
  extensions: readonly string[];
}

export const INTERPRETERS: readonly Interpreter[] = [
  { language: "python", programs: /^(python|python\d(\.\d+)?|pypy\d?)$/, inline: ["-c"], clusters: "c", values: ["-W", "-X", "--check-hash-based-pycs"], modules: ["-m"], stdin: true, extensions: [".py"] },
  { language: "javascript", programs: /^(node|nodejs)$/, inline: ["-e", "--eval", "-p", "--print"], clusters: "ep", loads: ["-r", "--require", "--import"], values: ["-r", "--require", "--import", "--loader", "--experimental-loader", "-C", "--conditions", "--input-type", "--env-file", "--title", "--max-old-space-size"], stdin: true, extensions: [".js", ".mjs", ".cjs"] },
  { language: "javascript", programs: /^deno$/, inline: [], subcommands: { eval: "inline", run: "script" }, values: ["--config", "-c", "--import-map", "--location", "--seed", "--v8-flags", "--env-file"], stdin: false, extensions: [".ts", ".js", ".mjs", ".tsx"] },
  { language: "javascript", programs: /^bun$/, inline: ["-e", "--eval", "-p", "--print"], subcommands: { run: "script" }, values: ["--cwd", "--config", "-c", "--env-file", "-r", "--preload"], stdin: false, extensions: [".ts", ".js", ".mjs", ".tsx"] },
  { language: "javascript", programs: /^(tsx|ts-node)$/, inline: ["-e", "--eval", "-p", "--print"], values: ["-r", "--require", "--tsconfig"], stdin: false, extensions: [".ts", ".tsx", ".js", ".mjs"] },
  { language: "ruby", programs: /^(ruby|jruby)$/, inline: ["-e"], clusters: "e", loads: ["-r"], values: ["-I", "-r", "-C", "-E", "--encoding"], stdin: true, extensions: [".rb"] },
  { language: "perl", programs: /^perl(\d[\d.]*)?$/, inline: ["-e", "-E"], clusters: "eE", loads: ["-M", "-m"], values: ["-I", "-M", "-m", "-x"], stdin: true, extensions: [".pl", ".pm"] },
  { language: "php", programs: /^php(\d[\d.]*)?$/, inline: ["-r"], values: ["-c", "-d", "-z"], stdin: true, extensions: [".php"] },
  { language: "applescript", programs: /^osascript$/, inline: ["-e"], values: ["-l", "-s"], stdin: true, extensions: [".applescript", ".scpt", ".js"] },
  { language: "lua", programs: /^(lua|luajit|lua5\.\d)$/, inline: ["-e"], values: ["-l"], stdin: true, extensions: [".lua"] },
  { language: "r", programs: /^(Rscript|R)$/, inline: ["-e"], values: ["--file"], stdin: true, extensions: [".r", ".R"] },
  // Round 5 step 2: the rest of the inline-code programs.
  { language: "julia", programs: /^julia$/, inline: ["-e", "--eval", "-E", "--print"], clusters: "eE", loads: [], values: ["-L", "--load", "-J", "--sysimage", "-t", "--threads", "-p", "--procs", "--project"], stdin: true, extensions: [".jl"] },
  { language: "powershell", programs: /^(pwsh|powershell|pwsh-preview)(\.exe)?$/i, inline: ["-c", "-C", "-Command", "-command", "-cmd"], opaqueInline: ["-e", "-ec", "-enc", "-EncodedCommand", "-encodedcommand"], values: ["-f", "-File", "-ExecutionPolicy", "-ep", "-WorkingDirectory", "-wd", "-OutputFormat", "-InputFormat", "-ConfigurationName", "-Version"], stdin: true, extensions: [".ps1", ".psm1"] },
  { language: "swift", programs: /^swift$/, inline: ["-e"], values: ["-I", "-L", "-F", "-module-name", "-sdk", "-target"], stdin: true, extensions: [".swift"] },
  { language: "tcl", programs: /^expect$/, inline: ["-c"], values: ["-D"], stdin: true, extensions: [".exp"] },
  { language: "awk", programs: /^(awk|gawk|mawk|nawk|busybox-awk)$/, inline: ["-e", "--source"], scripts: ["-f", "--file"], codeOperand: true, filter: true, values: ["-F", "-v", "--field-separator", "--assign", "-i", "--include", "-l", "--load"], stdin: false, extensions: [".awk"] },
  { language: "sed", programs: /^(sed|gsed)$/, inline: ["-e", "--expression"], scripts: ["-f", "--file"], codeOperand: true, filter: true, values: ["-l", "--line-length"], optionalValues: { "-i": /^$|^\.[\w.~-]+$/ }, stdin: false, extensions: [".sed"] },
  // A makefile's recipes are shell lines (`COMMAND_FORMS`); `make` alone reads ./Makefile, which isn't this line's code.
  { language: "make", programs: /^(make|gmake|bmake)$/, inline: [], scripts: ["-f", "--file", "--makefile"], values: ["-C", "--directory", "-j", "--jobs", "-l", "-o", "-W", "-I"], stdin: false, extensions: [".mk"] },
  // Languages Cypher has no capability table for: their code is opaque (`scanCode`), never local work.
  { language: "unread", programs: /^(guile|racket|elixir|iex|erl|escript|groovy|scala|scala-cli|clojure|clj|bb|ghc|runghc|runhaskell|ocaml|kotlin|kotlinc|dart|crystal|nim|v|zig|gst|sbcl|clisp|ecl|chicken|csi|gforth|io|pike|rexx|regina|scheme|chez|petite|janet|fennel|hy|xonsh|nu|elvish|ion)$/, inline: ["-c", "-e", "--eval", "-eval", "-E", "--expr", "-x"], values: [], stdin: true, extensions: [".scm", ".rkt", ".exs", ".erl", ".groovy", ".scala", ".clj", ".hs", ".ml", ".kts", ".dart", ".cr", ".nim", ".janet", ".fnl", ".hy", ".xsh", ".nu"] },
  { language: "tcl", programs: /^(tclsh|wish)(\d[\d.]*)?$/, inline: [], values: ["-encoding"], stdin: true, extensions: [".tcl"] },
  // A shell's `-c` is read by the shell evaluator itself (shell.ts); its scripts and its input are code here.
  { language: "shell", programs: /^(sh|bash|zsh|dash|ksh|fish)$/, inline: ["-c"], clusters: "c", values: ["-o", "+o", "-O", "+O", "--rcfile", "--init-file"], stdin: true, extensions: [".sh", ".bash", ".zsh"] },
];

/** The interpreter a program is, by `INTERPRETERS`. */
export function interpreterOf(program: string): Interpreter | undefined {
  const name = program.split("/").pop() ?? program;
  return INTERPRETERS.find((row) => row.programs.test(name));
}

/** What stands for the shell's words known only when they run, in code read with them (`patternText`). */
export const UNKNOWN_TEXT = "__cypher_unknown__";

/**
 * A word known only when it runs, as the code text it surely is (shell.ts `patterns`): its literal text,
 * the rest (substitutions, variables) as `UNKNOWN_TEXT`. Undefined when nothing of it is known.
 */
export function patternText(pattern: string | undefined): string | undefined {
  if (pattern === undefined) return undefined;
  const text = pattern
    // The environment's values are no code anyone can read here either.
    .replace(/\u0002[^\u0003]*\u0003/g, UNKNOWN_TEXT)
    .replaceAll("\u0001", UNKNOWN_TEXT)
    .replace(/\\(.)/gs, "$1");
  return text.split(UNKNOWN_TEXT).join("").trim() ? text : undefined;
}

/** The interpreter a script's `#!` line names (`#!/usr/bin/env python3`, `#!/bin/bash`). */
export function shebangInterpreter(text: string): Interpreter | undefined {
  const line = /^#!\s*(\S+)(?:\s+(?:-\S+\s+)*(\S+))?/.exec(text);
  if (!line) return undefined;
  const program = (line[1].split("/").pop() ?? "") === "env" ? line[2] : line[1];
  return program ? interpreterOf(program) : undefined;
}

/** Where a command's code comes from (`codeOf`). */
export type CodeSource =
  | { from: "inline"; text: string }
  | { from: "inline-unknown" }
  | { from: "stdin" }
  | { from: "script"; path: string }
  | { from: "script-unknown" };

/**
 * The code an interpreter command runs, by `INTERPRETERS`: its inline code, its script's path, or its
 * input. Undefined when it is no interpreter, or runs no code it reads (`python -m http.server`,
 * `node --version`).
 */
export function codeOf(words: readonly string[], unknown: string, patterns?: readonly (string | undefined)[]): { interpreter: Interpreter; source: CodeSource } | undefined {
  const interpreter = interpreterOf(words[0] ?? "");
  if (!interpreter || words[0] === unknown) return undefined;
  const args = words.slice(1);
  const offset = words.length - (patterns?.length ?? words.length);
  const inline: string[] = [];
  let unknownInline = false;
  let at = -1;
  const take = (value: string | undefined) => {
    // Code with words of the shell's known only when it runs (`-c "…open('$F')…"`): its known text, read
    // with a placeholder; code that is all of that (`-c "$CODE"`) can't be read.
    const known = value === unknown ? patternText(patterns?.[at + 1 - offset]) : value;
    if (known === undefined) unknownInline = true;
    else inline.push(known);
  };
  /** The modules its options load (`-MLWP::Simple`): read with its code. */
  const loaded: string[] = [];
  const withLoads = (source: CodeSource): CodeSource => (loaded.length && source.from === "inline" ? { from: "inline", text: `${loaded.map((module) => `use ${module};`).join("\n")}\n${source.text}` } : source);
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    const load = interpreter.loads?.find((flag) => word === flag || (word.startsWith(flag) && !word.startsWith("--") && word.length > flag.length) || word.startsWith(`${flag}=`));
    if (load) {
      const value = word === load ? args[++i] : word.slice(load.length).replace(/^=/, "");
      if (value !== undefined && value !== unknown) loaded.push(value.replace(/=.*$/, ""));
      continue;
    }
    if (word === unknown) {
      // A word known only when it runs where a script or flag could be: its code can't be read.
      return { interpreter, source: inline.length ? { from: "inline", text: inline.join("\n") } : { from: "inline-unknown" } };
    }
    if (word === "--") {
      const script = args[i + 1];
      if (inline.length) break;
      if (script === undefined) return interpreter.stdin ? { interpreter, source: { from: "stdin" } } : undefined;
      return { interpreter, source: script === unknown ? { from: "script-unknown" } : { from: "script", path: script } };
    }
    if (word.startsWith("-") && word !== "-") {
      if (interpreter.scripts?.includes(word)) {
        const path = args[i + 1];
        if (path === undefined) return undefined;
        return { interpreter, source: path === unknown ? { from: "script-unknown" } : { from: "script", path } };
      }
      if (interpreter.opaqueInline?.includes(word)) return { interpreter, source: { from: "inline-unknown" } };
      const optional = interpreter.optionalValues?.[word];
      if (optional) {
        if (args[i + 1] !== undefined && optional.test(args[i + 1])) i++;
        continue;
      }
      if (interpreter.inline.includes(word)) {
        at = ++i;
        take(args[i]);
        continue;
      }
      const long = /^(--[\w-]+)=(.*)$/s.exec(word);
      if (long && interpreter.inline.includes(long[1])) {
        take(long[2]);
        continue;
      }
      if (long) continue;
      // `-cCODE` (attached), or a cluster ending in a code flag (`-le CODE`).
      const short = interpreter.inline.find((flag) => /^-[A-Za-z]$/.test(flag) && word.startsWith(flag) && word.length > 2 && !word.startsWith("--"));
      if (short && !/^-[A-Za-z]+$/.test(word)) {
        take(word.slice(2));
        continue;
      }
      if (interpreter.clusters && /^-[A-Za-z]+$/.test(word) && interpreter.clusters.includes(word.at(-1)!)) {
        at = ++i;
        take(args[i]);
        continue;
      }
      if (interpreter.modules?.includes(word) || interpreter.modules?.some((flag) => word.startsWith(flag) && word.length > flag.length)) return inline.length ? { interpreter, source: { from: "inline", text: inline.join("\n") } } : undefined;
      if (interpreter.values.includes(word)) {
        i++;
        continue;
      }
      // `--version`, `-h`: no code.
      if (/^(-v|-V|--version|-h|--help)$/.test(word) && !inline.length) return undefined;
      continue;
    }
    if (inline.length || unknownInline) break;
    // `awk '{…}' file`: the first operand is the program, the rest is data.
    if (interpreter.codeOperand) {
      at = i;
      take(word);
      break;
    }
    const sub = interpreter.subcommands?.[word];
    if (sub === "inline") {
      at = i + 1;
      take(args[i + 1]);
      break;
    }
    if (sub === "script") {
      // `deno run [flags] x.ts`: the first word after its flags.
      for (let j = i + 1; j < args.length; j++) {
        const next = args[j];
        if (next === unknown) return { interpreter, source: { from: "script-unknown" } };
        if (next.startsWith("-")) {
          if (interpreter.values.includes(next)) j++;
          continue;
        }
        return { interpreter, source: { from: "script", path: next } };
      }
      return undefined;
    }
    if (interpreter.subcommands && !interpreter.inline.length) return undefined;
    if (word === "-") return interpreter.stdin ? { interpreter, source: { from: "stdin" } } : undefined;
    return { interpreter, source: { from: "script", path: word } };
  }
  if (inline.length) return { interpreter, source: withLoads({ from: "inline", text: inline.join("\n") }) };
  if (unknownInline) return { interpreter, source: { from: "inline-unknown" } };
  // No script: its input is its program (`python3 <<EOF`, `echo … | node`), when it reads one.
  if (interpreter.subcommands && !interpreter.inline.length) return undefined;
  return interpreter.stdin ? { interpreter, source: { from: "stdin" } } : undefined;
}

// --- What code can do --------------------------------------------------------------------

export type Capability = "network" | "spawn" | "read" | "write" | "env" | "home" | "dynamic";

/**
 * Per language, the names that give code a capability (data). A name is matched as an identifier
 * (`requests`, `http.client`, `Net::HTTP`, `curl_init`); `(` at its end asks for a call. `"*"` rows
 * hold for every language.
 */
export const CAPABILITIES: readonly { capability: Capability; languages: readonly Language[] | "*"; names: readonly string[] }[] = [
  // Reaching the network.
  { capability: "network", languages: ["python"], names: ["urllib", "urllib2", "urllib3", "urlopen", "requests", "httpx", "aiohttp", "http.client", "httplib", "socket", "smtplib", "ftplib", "telnetlib", "xmlrpc", "paramiko", "pycurl", "websocket", "websockets", "grpc", "boto3", "botocore", "asyncssh", "fabric"] },
  { capability: "network", languages: ["javascript"], names: ["fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "sendBeacon", "node:http", "node:https", "node:http2", "node:net", "node:tls", "node:dgram", "require('http')", "require('https')", "require('net')", "require('tls')", "require('dgram')", 'require("http")', 'require("https")', 'require("net")', 'require("tls")', 'require("dgram")', "http.request", "https.request", "http.get", "https.get", "net.connect", "net.createConnection", "tls.connect", "axios", "node-fetch", "undici", "superagent", "needle", "got(", "ky(", "Deno.connect", "Deno.connectTls", "Bun.connect", "Bun.fetch", "socket.io-client"] },
  { capability: "network", languages: ["ruby"], names: ["Net::HTTP", "Net::SMTP", "Net::FTP", "Net::SSH", "open-uri", "URI.open", "TCPSocket", "UDPSocket", "Socket.tcp", "HTTParty", "Faraday", "RestClient", "Excon", "Typhoeus", "HTTP.post", "HTTP.get"] },
  { capability: "network", languages: ["perl"], names: ["LWP", "LWP::UserAgent", "LWP::Simple", "HTTP::Tiny", "HTTP::Request", "IO::Socket", "IO::Socket::INET", "IO::Socket::SSL", "Net::HTTP", "Net::SMTP", "Net::FTP", "Socket", "Mojo::UserAgent", "WWW::Mechanize", "Furl"] },
  { capability: "network", languages: ["php"], names: ["curl_init", "curl_exec", "fsockopen", "pfsockopen", "stream_socket_client", "socket_create", "socket_connect", "mail(", "GuzzleHttp", "ftp_connect", "SoapClient"] },
  { capability: "network", languages: ["applescript"], names: ["open location", "NSURLSession", "NSURLConnection", "NSURLRequest", "NSMutableURLRequest", "dataWithContentsOfURL", "stringWithContentsOfURL"] },
  { capability: "network", languages: ["lua"], names: ["socket.http", "ssl.https", "require('socket')", 'require("socket")', "luasocket", "http.request"] },
  { capability: "network", languages: ["r"], names: ["httr", "httr2", "RCurl", "curl::", "download.file", "url(", "POST(", "GET(", "socketConnection", "req_perform"] },
  { capability: "network", languages: ["tcl"], names: ["socket", "http::geturl", "tls::socket"] },
  { capability: "network", languages: ["julia"], names: ["Downloads", "HTTP.", "Sockets", "connect(", "download(", "LibCURL"] },
  { capability: "network", languages: ["powershell"], names: ["Invoke-WebRequest", "Invoke-RestMethod", "iwr", "irm", "Net.WebClient", "System.Net.", "Net.Sockets", "Send-MailMessage", "Start-BitsTransfer", "HttpClient", "WebRequest"] },
  { capability: "network", languages: ["swift"], names: ["URLSession", "URLRequest", "NSURLConnection", "NWConnection", "CFStream", "URL(string"] },
  { capability: "network", languages: ["awk"], names: ["/inet/", "/inet4/", "/inet6/"] },
  // Running a program (its words are read as a shell line).
  { capability: "spawn", languages: ["python"], names: ["subprocess", "os.system", "os.popen", "os.exec", "os.execv", "os.execvp", "os.spawn", "os.spawnv", "pty.spawn", "commands.getoutput", "sh.", "plumbum", "Popen"] },
  { capability: "spawn", languages: ["javascript"], names: ["child_process", "node:child_process", "execSync", "exec(", "execFile", "execFileSync", "spawn(", "spawnSync", "Deno.Command", "Deno.run", "Bun.spawn", "Bun.spawnSync", "Bun.$", "$`", "execa", "zx"] },
  { capability: "spawn", languages: ["ruby"], names: ["system(", "exec(", "spawn(", "Open3", "IO.popen", "Kernel.exec", "Kernel.system", "%x", "`", "Process.spawn", "PTY.spawn"] },
  { capability: "spawn", languages: ["perl"], names: ["system(", "system ", "exec(", "exec ", "qx", "`", "open3", "IPC::Open3", "IPC::Run", "Capture::Tiny"] },
  { capability: "spawn", languages: ["php"], names: ["shell_exec", "exec(", "system(", "passthru", "proc_open", "popen(", "`", "pcntl_exec"] },
  { capability: "spawn", languages: ["applescript"], names: ["do shell script", "doShellScript", "NSTask", "Application('Terminal')"] },
  { capability: "spawn", languages: ["lua"], names: ["os.execute", "io.popen"] },
  { capability: "spawn", languages: ["r"], names: ["system(", "system2(", "shell(", "processx"] },
  { capability: "spawn", languages: ["tcl"], names: ["exec", "open |", "spawn"] },
  { capability: "spawn", languages: ["julia"], names: ["run(", "pipeline(", "`"] },
  { capability: "spawn", languages: ["powershell"], names: ["Start-Process", "Invoke-Command", "cmd /c", "cmd.exe", "& "] },
  { capability: "spawn", languages: ["swift"], names: ["Process(", "NSTask", "system(", "posix_spawn", "popen("] },
  { capability: "spawn", languages: ["awk"], names: ["system(", "|&", "|"] },
  // Reading a file.
  { capability: "read", languages: ["python"], names: ["open(", "io.open", "read_text", "read_bytes", "Path(", "pathlib", "codecs.open", "fileinput", "shutil.copy", "dotenv", "load_dotenv", "dotenv_values", "configparser", "json.load", "yaml.safe_load", "toml.load", "np.load", "pd.read_csv", "linecache", "mmap"] },
  { capability: "read", languages: ["javascript"], names: ["readFileSync", "readFile(", "createReadStream", "fs.read", "fs/promises", "node:fs", "require('fs')", 'require("fs")', "Deno.readTextFile", "Deno.readTextFileSync", "Deno.readFile", "Deno.readFileSync", "Deno.open", "Deno.openSync", "Bun.file", "dotenv", "require(", "import("] },
  { capability: "read", languages: ["ruby"], names: ["File.read", "File.open", "File.readlines", "File.foreach", "IO.read", "IO.readlines", "Dotenv", "YAML.load_file", "JSON.load_file", "Pathname", "open("] },
  { capability: "read", languages: ["perl"], names: ["open(", "open ", "File::Slurp", "Path::Tiny", "read_file", "slurp", "<STDIN>", "do "] },
  { capability: "read", languages: ["php"], names: ["file_get_contents", "fopen", "file(", "readfile", "fread", "parse_ini_file", "include", "require", "Dotenv", "SplFileObject"] },
  { capability: "read", languages: ["applescript"], names: ["read file", "read POSIX file", "read (POSIX file", "stringWithContentsOfFile", "contentsOfFile", "NSFileManager"] },
  { capability: "read", languages: ["lua"], names: ["io.open", "io.lines", "io.read", "dofile", "loadfile"] },
  { capability: "read", languages: ["r"], names: ["readLines", "read.csv", "read.table", "readRDS", "scan(", "file(", "readRenviron", "readChar", "readBin", "source("] },
  { capability: "read", languages: ["tcl"], names: ["open", "read", "fcopy", "source"] },
  { capability: "read", languages: ["julia"], names: ["open(", "read(", "readlines(", "readline(", "eachline(", "include("] },
  { capability: "read", languages: ["powershell"], names: ["Get-Content", "gc ", "-InFile", "ReadAllText", "ReadAllBytes", "ReadAllLines", "Import-Csv", "type ", "cat "] },
  { capability: "read", languages: ["swift"], names: ["contentsOf", "contentsOfFile", "FileManager", "FileHandle", "fopen("] },
  { capability: "read", languages: ["awk"], names: ["getline"] },
  // Writing a file (round 5 step 2: with a permission-system path among its literals, the floor).
  { capability: "write", languages: ["python"], names: ["'w'", '"w"', "'a'", '"a"', "'wb'", '"wb"', "write_text", "write_bytes", ".write_text", ".write_bytes", "shutil.copy", "shutil.copyfile", "shutil.move", "os.rename", "os.replace", "os.remove", "os.unlink", "os.symlink", "os.link"] },
  { capability: "write", languages: ["javascript"], names: [".writeFile", ".writeFileSync", ".appendFile", ".createWriteStream", ".rename", ".unlink", ".rm", ".copyFile", ".symlink", ".link", "writeFile", "writeFileSync", "appendFile", "appendFileSync", "createWriteStream", "renameSync", "rename(", "unlinkSync", "rmSync", "copyFile", "copyFileSync", "symlinkSync", "linkSync", "Deno.writeTextFile", "Deno.writeFile", "Bun.write"] },
  { capability: "write", languages: ["ruby"], names: ["File.write", "IO.write", "'w'", '"w"', "'a'", '"a"', "FileUtils", "File.rename", "File.delete", "File.symlink"] },
  { capability: "write", languages: ["perl"], names: ["'>", '">', "'>>", '">>', "unlink", "rename", "symlink"] },
  { capability: "write", languages: ["php"], names: ["file_put_contents", "fwrite", "'w'", '"w"', "rename(", "unlink(", "copy("] },
  { capability: "write", languages: ["julia"], names: ["write(", "\"w\"", "cp(", "mv(", "rm("] },
  { capability: "write", languages: ["powershell"], names: ["Set-Content", "Add-Content", "Out-File", "WriteAllText", "Copy-Item", "Move-Item", "Remove-Item", ">"] },
  { capability: "write", languages: ["swift"], names: ["write(to", "createFile", "moveItem", "copyItem", "removeItem"] },
  { capability: "write", languages: ["lua", "r", "tcl", "applescript", "awk", "sed"], names: ["io.open", "writeLines", "write(", "file(", "open", "write ", "> \"", ">\"", "w "] },
  // The environment (a credential variable read by its name; or the whole of it).
  { capability: "env", languages: ["python"], names: ["os.environ", "os.getenv", "environ.get", "getenv("] },
  { capability: "env", languages: ["javascript"], names: ["process.env", "Deno.env", "Bun.env"] },
  { capability: "env", languages: ["ruby"], names: ["ENV"] },
  { capability: "env", languages: ["perl"], names: ["$ENV", "%ENV"] },
  { capability: "env", languages: ["php"], names: ["getenv(", "$_ENV", "$_SERVER"] },
  { capability: "env", languages: ["applescript"], names: ["system attribute"] },
  { capability: "env", languages: ["lua"], names: ["os.getenv"] },
  { capability: "env", languages: ["r"], names: ["Sys.getenv"] },
  { capability: "env", languages: ["tcl"], names: ["$env", "$::env", "env("] },
  { capability: "env", languages: ["julia"], names: ["ENV"] },
  { capability: "env", languages: ["powershell"], names: ["$env:", "GetEnvironmentVariable", "Get-ChildItem env:", "gci env:", "ls env:", "dir env:"] },
  { capability: "env", languages: ["swift"], names: ["environment", "getenv("] },
  { capability: "env", languages: ["awk"], names: ["ENVIRON"] },
  // The home folder (a path joined to it: `os.path.expanduser('~') + '/.ssh/id_rsa'`).
  { capability: "home", languages: ["python"], names: ["expanduser", "Path.home", "os.environ['HOME']", 'os.environ["HOME"]', "getenv('HOME')", 'getenv("HOME")'] },
  { capability: "home", languages: ["javascript"], names: ["homedir(", "os.homedir", "process.env.HOME", "Deno.env.get('HOME')", 'Deno.env.get("HOME")'] },
  { capability: "home", languages: ["ruby"], names: ["Dir.home", "ENV['HOME']", 'ENV["HOME"]', "File.expand_path"] },
  { capability: "home", languages: ["perl"], names: ["$ENV{HOME}", "File::HomeDir", "glob('~"] },
  { capability: "home", languages: ["php"], names: ["getenv('HOME')", 'getenv("HOME")', "$_SERVER['HOME']"] },
  { capability: "home", languages: ["applescript"], names: ["path to home folder", "NSHomeDirectory", "system attribute \"HOME\""] },
  { capability: "home", languages: ["lua"], names: ["os.getenv('HOME')", 'os.getenv("HOME")'] },
  { capability: "home", languages: ["r"], names: ["path.expand", "Sys.getenv('HOME')", 'Sys.getenv("HOME")', "normalizePath('~"] },
  { capability: "home", languages: ["julia"], names: ["homedir("] },
  { capability: "home", languages: ["powershell"], names: ["$HOME", "$env:HOME", "$env:USERPROFILE", "~/"] },
  { capability: "home", languages: ["swift"], names: ["homeDirectoryForCurrentUser", "NSHomeDirectory", "expandingTildeInPath"] },
  { capability: "home", languages: ["awk"], names: ["ENVIRON[\"HOME\"]"] },
  // Code built or decoded at run time: what runs can't be read here.
  { capability: "dynamic", languages: ["python"], names: ["exec(", "eval(", "compile(", "__import__(", "importlib", "b64decode", "b32decode", "a85decode", "b85decode", "codecs.decode", "marshal.loads", "pickle.loads", "zlib.decompress", "bytes.fromhex", "rot13", "getattr(__builtins__"] },
  { capability: "dynamic", languages: ["javascript"], names: ["eval(", "Function(", "new Function", "vm.runIn", "vm.Script", "runInThisContext", "atob(", "'base64')", '"base64")', "fromCharCode", "require(process", "import(process"] },
  { capability: "dynamic", languages: ["ruby"], names: ["eval(", "eval ", "instance_eval", "class_eval", "module_eval", "send(", "public_send", "Base64.decode64", "unpack1", "Marshal.load", "const_get"] },
  { capability: "dynamic", languages: ["perl"], names: ["eval ", "eval(", "eval{", "decode_base64", "MIME::Base64", "pack(", "unpack("] },
  { capability: "dynamic", languages: ["php"], names: ["eval(", "base64_decode", "gzinflate", "gzuncompress", "str_rot13", "create_function", "assert(", "call_user_func", "preg_replace_callback", "hex2bin"] },
  { capability: "dynamic", languages: ["applescript"], names: ["run script", "load script"] },
  { capability: "dynamic", languages: ["lua"], names: ["load(", "loadstring", "string.char"] },
  { capability: "dynamic", languages: ["r"], names: ["eval(", "parse(text", "base64enc", "rawToChar"] },
  { capability: "dynamic", languages: ["tcl"], names: ["eval", "uplevel", "binary decode", "subst"] },
  { capability: "dynamic", languages: ["julia"], names: ["eval(", "Meta.parse", "include_string", "base64decode", "@eval"] },
  { capability: "dynamic", languages: ["powershell"], names: ["Invoke-Expression", "iex", "FromBase64String", "ScriptBlock", "-EncodedCommand"] },
  { capability: "dynamic", languages: ["swift"], names: ["dlopen", "dlsym", "NSClassFromString", "base64Encoded"] },
];

/**
 * The home folder in any syntax, across shells and languages (round 6, W4b): one table of forms, not a name per
 * language. A literal that starts with one (`"$ENV{HOME}/.pypirc"`, `f"{Path.home()}/.netrc"`,
 * `` `${process.env.HOME}/.npmrc` ``, `"#{Dir.home}/.aws"`) is read as `~/…`, and code that uses any of them
 * finds the home folder (its other literals may be home-relative).
 */
export const HOME_FORMS: readonly RegExp[] = [
  /\$\{?HOME\}?(?![\w])/,
  /\$\{?(?:env:)?(?:HOME|USERPROFILE)\}?/i,
  /\$ENV\s*\{\s*['"]?HOME['"]?\s*\}/,
  /%(?:HOME|USERPROFILE)%/i,
  /\bENV\s*\[\s*['"]HOME['"]\s*\]/,
  /\bENV\s*\.fetch\s*\(\s*['"]HOME['"]/,
  /\bENVIRON\s*\[\s*['"]HOME['"]\s*\]/,
  /\b(?:os\.)?environ(?:\.get)?\s*[[(]\s*['"]HOME['"]\s*[\])]/,
  /\b(?:os\.|Sys\.|_?w?)?getenv\s*\(\s*['"]HOME['"]\s*\)/i,
  /\b(?:process|Bun)\.env(?:\.HOME\b|\s*\[\s*['"]HOME['"]\s*\])/,
  /\bDeno\.env\.get\s*\(\s*['"]HOME['"]\s*\)/,
  /\b(?:os\.)?homedir\s*\(\s*\)/,
  /\bPath\.home\s*\(\s*\)/,
  /\bexpanduser\s*\(\s*['"]~['"]\s*\)/,
  /\bDir\.home\b/,
  /\bNSHomeDirectory\s*\(\s*\)/,
  /\bFileManager\.default\.homeDirectoryForCurrentUser\b/,
  /\$_SERVER\s*\[\s*['"]HOME['"]\s*\]/,
  /\[Environment\]::GetFolderPath\(\s*['"]?UserProfile['"]?\s*\)/i,
  /\bSystem\.getProperty\s*\(\s*['"]user\.home['"]\s*\)/,
  /\bstd::env::home_dir\s*\(\s*\)|\bdirs::home_dir\s*\(\s*\)|\bos\.UserHomeDir\s*\(\s*\)/,
];

/** An interpolation around a home form inside a literal: `#{…}`, `${…}`, `{…}` (f-strings), `$(…)`. */
const HOME_AT_START = new RegExp(`^(?:#\\{|\\$\\{|\\{|\\$\\()?\\s*(?:${HOME_FORMS.map((form) => form.source).join("|")})\\s*[})]?`);

/** A literal's text with a leading home form read as `~` (`"$ENV{HOME}/.pypirc"` → `~/.pypirc`); else as it is. */
export function homeRelative(text: string): string {
  const match = HOME_AT_START.exec(text);
  if (!match || match[0].length === 0) return text;
  const rest = text.slice(match[0].length);
  return rest === "" || rest.startsWith("/") || rest.startsWith("\\") ? `~${rest.replace(/^\\/, "/")}` : text;
}

/** A home form joined to what follows it (`os.environ['HOME'] + '/.pypirc'`, `path.join(os.homedir(), '.npmrc')`, `Path.home() / '.netrc'`). */
const HOME_JOINED = new RegExp(`(?:${HOME_FORMS.map((form) => form.source).join("|")})\\s*(?:\\+|\\.\\.|\\.|,|/|&|\\.\\s*)\\s*$`);

/** The literals of code that follow a home form they are joined to, as `~/…`. */
export function homeJoined(code: string, literals: readonly Literal[]): string[] {
  const out: string[] = [];
  for (const literal of literals) {
    const before = code.slice(Math.max(0, literal.at - 120), literal.at);
    if (HOME_JOINED.test(before) && literal.text && !/^[a-z][\w+.-]*:\/\//i.test(literal.text)) out.push(`~/${literal.text.replace(/^[/\\]+/, "")}`);
  }
  return out;
}

/** Whether code finds the home folder by any form (`HOME_FORMS`). */
export function usesHome(code: string): boolean {
  return HOME_FORMS.some((form) => form.test(code));
}

/**
 * Network capability by category, in any language (round 6, W4c): raw sockets, socket devices, HTTP
 * clients, DNS lookups, and host:port literals of another machine. With the names of `CAPABILITIES`, not
 * instead of them: a library the table doesn't name still opens a socket, speaks HTTP or resolves a name.
 */
export const NETWORK_CATEGORIES: readonly { category: string; form: RegExp }[] = [
  { category: "a raw socket", form: /\b(?:socket\s*\(|socket\.socket|Socket(?:::|\.|\s*\()|TCPSocket|UDPSocket|TcpClient|UdpClient|TCPServer|SOCK_STREAM|SOCK_DGRAM|AF_INET6?|NWConnection|net\.Dial|DatagramSocket|SocketChannel|ServerSocket|fsockopen|stream_socket_client|IO::Socket)/ },
  { category: "a socket device", form: /\/dev\/(?:tcp|udp)\// },
  { category: "an HTTP client", form: /\b(?:Http(?:s)?(?:Client|Connection|Request|URLConnection)|HTTPS?Connection|WebClient|WebRequest|URLSession|urlopen|curl_easy|libcurl|reqwest|ureq|http\.(?:Get|Post|NewRequest)|[Hh]ttps?\.(?:get|post|put|request)\s*\()/ },
  { category: "a DNS lookup", form: /\b(?:gethostbyname|getaddrinfo|dns\.(?:lookup|resolve\w*)|Resolv(?:::DNS)?|Net::DNS|InetAddress\.getByName|LookupHost|res_query)\b/ },
];

/** A host:port literal naming another machine (not this one): `"198.51.100.23:9001"`, `"exfil.example.com:443"`. */
const HOST_PORT = /^(?:(?:\d{1,3}\.){3}\d{1,3}|(?:[a-z0-9-]+\.)+[a-z]{2,}|\[[0-9a-f:]+\]):\d{2,5}$/i;
const THIS_MACHINE = /^(?:127\.|0\.0\.0\.0|localhost|\[::1\]|::1)/i;

/** The network categories code uses (`NETWORK_CATEGORIES`, host:port literals). */
export function networkCategories(code: string, literals: readonly { text: string }[]): string[] {
  const out = NETWORK_CATEGORIES.filter((row) => row.form.test(code)).map((row) => row.category);
  if (literals.some((literal) => HOST_PORT.test(literal.text.trim()) && !THIS_MACHINE.test(literal.text.trim()))) out.push("a host:port of another machine");
  return out;
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** A name of `CAPABILITIES` as a regex: an identifier, not part of a longer one; `(` at its end allows spaces before it. */
function nameRegex(name: string): RegExp {
  const call = name.endsWith("(");
  const body = escapeRegex(call ? name.slice(0, -1) : name);
  const start = /^[\w$]/.test(name) ? "(?<![\\w$.])" : "";
  const end = call ? "\\s*\\(" : /[\w$]$/.test(name) ? "(?![\\w$])" : "";
  return new RegExp(`${start}${body}${end}`);
}

const NAME_CACHE = new Map<string, RegExp>();
function nameMatches(name: string, text: string): boolean {
  let regex = NAME_CACHE.get(name);
  if (!regex) NAME_CACHE.set(name, (regex = nameRegex(name)));
  return regex.test(text);
}

/** The names of `capability` for `language` the code uses. */
export function capabilityNames(text: string, language: Language, capability: Capability): string[] {
  const names: string[] = [];
  for (const row of CAPABILITIES) {
    if (row.capability !== capability || (row.languages !== "*" && !row.languages.includes(language))) continue;
    for (const name of row.names) if (nameMatches(name, text) && !names.includes(name)) names.push(name);
  }
  return names;
}

/** A string literal of code: its text, where it starts, and whether it is a group's member (`["curl", "-d", …]`). */
export interface Literal {
  text: string;
  at: number;
  end: number;
}

/** The string literals of code (single, double, backtick, triple quotes), escapes read. */
export function literalsOf(code: string): Literal[] {
  const out: Literal[] = [];
  for (let i = 0; i < code.length; i++) {
    const char = code[i];
    if (char !== "'" && char !== '"' && char !== "`") continue;
    const triple = code.startsWith(char.repeat(3), i);
    const quote = triple ? char.repeat(3) : char;
    let text = "";
    let j = i + quote.length;
    for (; j < code.length; j++) {
      if (code[j] === "\\" && j + 1 < code.length) {
        const next = code[j + 1];
        text += next === "n" ? "\n" : next === "t" ? "\t" : next;
        j++;
        continue;
      }
      if (code.startsWith(quote, j)) break;
      if (!triple && char !== "`" && code[j] === "\n") break;
      text += code[j];
    }
    out.push({ text, at: i, end: j + quote.length });
    i = j + quote.length - 1;
  }
  return out;
}

/** Literals next to each other in one list or call (`["curl", "-d", "@.env", url]`, `"a", "b"`): one argv each. */
export function argvGroups(code: string, literals: readonly Literal[]): string[][] {
  const groups: string[][] = [];
  let current: Literal[] = [];
  for (const literal of literals) {
    const last = current.at(-1);
    if (last && /^\s*,\s*$/.test(code.slice(last.end, literal.at))) current.push(literal);
    else {
      if (current.length > 1) groups.push(current.map((item) => item.text));
      current = [literal];
    }
  }
  if (current.length > 1) groups.push(current.map((item) => item.text));
  return groups;
}

/** Literals joined into one path (`'~' + '/.ssh/id_rsa'`, `os.path.join(home, '.aws', 'credentials')`, `Path.home() / '.netrc'`). */
export function joinedPaths(code: string, literals: readonly Literal[]): string[] {
  const out: string[] = [];
  let current: Literal[] = [];
  /** The current run of literals joined by concatenation (`+`, `..`, `.`, `&`): one string, its text as written (round 5 step 2). */
  let run: string[] = [];
  const endRun = () => {
    if (run.length > 1) out.push(run.join(""));
    run = [];
  };
  const flush = () => {
    if (current.length > 1) {
      const parts = current.map((item) => item.text.replace(/^\/+|\/+$/g, "")).filter(Boolean);
      out.push(`${current[0].text.startsWith("/") ? "/" : ""}${parts.join("/")}`);
    }
    endRun();
    current = [];
  };
  for (const literal of literals) {
    const last = current.at(-1);
    const between = last ? code.slice(last.end, literal.at) : "";
    if (last && /^\s*(?:,|\+|\/|\.\.|\.|&)\s*$/.test(between)) {
      if (/[,/]/.test(between)) {
        endRun();
        run = [literal.text];
      } else run.push(literal.text);
      current.push(literal);
    } else {
      flush();
      current = [literal];
      run = [literal.text];
    }
  }
  flush();
  return out;
}

/**
 * Per language, forms whose first group is a shell line the code runs (data, round 5 step 2): sed's `e`
 * command (`1e curl …`), julia's backtick commands. The `opaque` forms run text that is computed (sed's
 * `s///e`, which runs the line it makes).
 */
export const COMMAND_FORMS: Partial<Record<Language, { runs?: readonly RegExp[]; opaque?: readonly { form: RegExp; why: string }[] }>> = {
  sed: {
    runs: [/(?:^|[;\n{}]|[0-9$/!])\s*e\s+([^;\n}]+)/g],
    opaque: [{ form: /(?:^|[;\n{}]|[0-9$/!])\s*s(.)(?:\\.|(?!\1).)*\1(?:\\.|(?!\1).)*\1[gpIiMm0-9w]*e/, why: "it runs the text it substitutes as a command (sed `s///e`)" }, { form: /(?:^|[;\n{}]|[0-9$/!])\s*e\s*(?:$|[;\n}])/, why: "it runs its input lines as commands (sed `e`)" }],
  },
  julia: { runs: [/`([^`]+)`/g] },
  make: { runs: [/^\t+@?-?(.+)$/gm] },
};

/** Languages whose paths are bare words, not only string literals (`-InFile .env`). */
const BARE_PATHS: ReadonlySet<Language> = new Set(["powershell"]);

/** How much escaped or computed text makes code obfuscated: more than this many escapes, or computed characters. */
const MAX_ESCAPES = 8;

/** What reading code found (`scanCode`). */
export interface CodeScan {
  /** Names that reach the network (`requests`, `urlopen`). */
  network: string[];
  /** Names that run a program; with `commands`, the shell lines they run. */
  spawn: string[];
  /** Shell lines the code runs (each literal of a spawning call, each argv list), to read as lines of their own. */
  commands: string[];
  /** Literals that may name files it reads (paths, joined paths, from home). */
  paths: string[];
  /** Whether it reads files at all (`open(`, `readFileSync`). */
  reads: boolean;
  /** Whether it writes, moves or deletes files (`'w'`, `writeFileSync`). */
  writes: boolean;
  /** Credential variables it reads by name, or "the environment" for all of it. */
  env: string[];
  /** Why it can't be read: names that build code at run time, obfuscation. */
  opaque: string[];
  /** URL literals (`https://…`): with a reader, a way to reach the network. */
  urls: string[];
}

/** After an environment name, what it reads: a variable's name, or nothing (all of it). */
const ENV_NAME = /^\s*(?:\[\s*|\.get\s*\(\s*|\.fetch\s*\(\s*|\(\s*|\{\s*|\.|->\s*)?['"]?([A-Za-z_][A-Za-z0-9_]*)/;
/** Reading the whole environment: passing it on, serialising it, iterating it. */
const ENV_ALL = /^\s*(?:[),;}\]]|$|\.(?:items|keys|values|copy|to_h|to_a|each|entries|toObject)\b)/;

/**
 * What code does, by `CAPABILITIES`: its network names, the programs it runs (as shell lines), the paths
 * its literals may name, the credential variables it reads, and what makes it unreadable.
 * `secretName` decides what a credential's variable is called (risk.ts).
 */
export function scanCode(code: string, language: Language, secretName: (name: string) => boolean): CodeScan {
  const literals = literalsOf(code);
  const scan: CodeScan = { network: [], spawn: [], commands: [], paths: [], reads: false, writes: false, env: [], opaque: [], urls: [] };
  if (language === "unread") {
    scan.opaque.push("it is code in a language Cypher can't read");
    return scan;
  }
  const forms = COMMAND_FORMS[language];
  for (const regex of forms?.runs ?? []) for (const match of code.matchAll(regex)) if (match[1]?.trim()) scan.commands.push(match[1].trim());
  for (const item of forms?.opaque ?? []) if (item.form.test(code)) scan.opaque.push(item.why);
  scan.network = capabilityNames(code, language, "network");
  if (language !== "sed") for (const category of networkCategories(code, literals)) if (!scan.network.includes(category)) scan.network.push(category);
  scan.spawn = capabilityNames(code, language, "spawn");
  if (scan.commands.length && !scan.spawn.length) scan.spawn.push("a command form");
  scan.reads = capabilityNames(code, language, "read").length > 0;
  scan.writes = capabilityNames(code, language, "write").length > 0;
  const dynamic = capabilityNames(code, language, "dynamic");
  if (dynamic.length) scan.opaque.push(`it builds or decodes code at run time (${dynamic.slice(0, 3).join(", ")})`);
  const escapes = (code.match(/\\x[0-9a-fA-F]{2}|\\u[0-9a-fA-F]{4}|\\[0-7]{3}|\bchr\s*\(|\bString\.fromCharCode|\\u\{[0-9a-fA-F]+\}/g) ?? []).length;
  if (escapes > MAX_ESCAPES) scan.opaque.push(`its text is escaped or computed (${escapes} escapes)`);
  // Credential variables, and the whole environment.
  for (const row of CAPABILITIES) {
    if (row.capability !== "env" || (row.languages !== "*" && !row.languages.includes(language))) continue;
    for (const name of row.names) {
      const regex = new RegExp(nameRegex(name).source, "g");
      for (const match of code.matchAll(regex)) {
        const rest = code.slice((match.index ?? 0) + match[0].length, (match.index ?? 0) + match[0].length + 60);
        const variable = ENV_NAME.exec(rest)?.[1];
        if (variable && secretName(variable)) scan.env.push(variable);
        else if (!variable || ENV_ALL.test(rest)) {
          if (ENV_ALL.test(rest) || name.startsWith("%")) scan.env.push("the environment");
        }
      }
    }
  }
  // Paths its literals may name: as written, joined, and from home when it finds the home folder.
  const home = capabilityNames(code, language, "home").length > 0 || usesHome(code);
  const bare = BARE_PATHS.has(language) ? [...code.matchAll(/(?<=^|[\s=(,])(?:~|\.{1,2})?\/?[\w.-]*[./][\w.-]+(?:\/[\w.-]+)*/g)].map((match) => match[0]).filter((word) => !/^[\w-]+\.[A-Z]/.test(word)) : [];
  for (const text of [...literals.map((literal) => literal.text), ...joinedPaths(code, literals), ...bare].map(homeRelative).concat(homeJoined(code, literals))) {
    if (/^[a-z][\w+.-]*:\/\//i.test(text)) {
      scan.urls.push(text);
      continue;
    }
    if (!text || text.length > 400 || /\s{2,}|\n/.test(text)) continue;
    scan.paths.push(text);
    if (home && !text.startsWith("/") && !text.startsWith("~")) scan.paths.push(`~/${text.replace(/^\.\//, "")}`);
  }
  // What it runs: each literal of the code that reads as a command line, and each argv list.
  if (scan.spawn.length) {
    for (const literal of literals) if (/\s/.test(literal.text.trim()) || /^[\w./-]+$/.test(literal.text)) scan.commands.push(literal.text);
    for (const group of argvGroups(code, literals)) scan.commands.push(group.map((word) => (/^[\w@%+=:,./-]+$/.test(word) ? word : `'${word.replaceAll("'", `'\\''`)}'`)).join(" "));
  }
  return scan;
}

// --- Options whose value is a command ---------------------------------------------------

/**
 * Programs whose options take a command line they run (data, round 5 step 2): `man -P 'sh -c …'`,
 * `rsync -e '…'`, `tar --to-command=…`, `ssh -o ProxyCommand=…`. Their values are read as shell lines
 * (core.ts `codeFinding`), so what they send and read is the floor's like any other line's.
 * `keys`: for `-o Key=Value` programs, the keys whose value is a command.
 */
export const COMMAND_OPTIONS: readonly { programs: RegExp; options?: readonly string[]; keyOption?: string; keys?: RegExp }[] = [
  { programs: /^man$/, options: ["-P", "--pager", "-H", "--html"] },
  { programs: /^rsync$/, options: ["-e", "--rsh", "--rsync-path"] },
  { programs: /^(ssh|scp|sftp)$/, keyOption: "-o", keys: /^(ProxyCommand|LocalCommand|KnownHostsCommand|RemoteCommand)$/i },
  { programs: /^(tar|gtar|bsdtar)$/, options: ["-I", "--use-compress-program", "--to-command", "--info-script", "--new-volume-script", "-F", "--checkpoint-action"] },
  { programs: /^zip$/, options: ["-TT", "--unzip-command"] },
  { programs: /^(less|more)$/, options: ["--lesskey-src"] },
  { programs: /^(vim|vi|nvim|ex)$/, options: ["-c", "--cmd"] },
  { programs: /^git$/, keyOption: "-c" },
  { programs: /^(npm|pnpm)$/ },
];

/**
 * git configuration keys whose value is a command git runs (data, round 5 step 2): set by `git -c k=v`
 * or `git config k v`. `alias.*` and `credential.helper` run a shell line when the value starts with `!`.
 */
export const GIT_COMMAND_KEYS =
  /^(core\.(pager|sshcommand|editor|fsmonitor|askpass)|sequence\.editor|diff\.external|diff\.[^.]+\.(textconv|command)|merge\.[^.]+\.driver|filter\.[^.]+\.(clean|smudge|process)|credential\.helper|credential\.[^.]+\.helper|gpg\.program|gpg\.[^.]+\.program|pager\.[^.]+|alias\.[^.]+|uploadpack\.packobjectshook|interactive\.difffilter|instaweb\.httpd|web\.browser|browser\.[^.]+\.cmd|man\.[^.]+\.cmd|sendemail\.(smtpserver|tocmd|cccmd)|remote\.[^.]+\.(uploadpack|receivepack|proxy)|core\.gitproxy)$/i;

/** A git configuration value as the shell line it runs (`!cmd` for aliases and helpers), or undefined. */
function gitCommandValue(key: string, value: string): string | undefined {
  if (!GIT_COMMAND_KEYS.test(key)) return undefined;
  if (/^(alias|credential)\./i.test(key)) return value.startsWith("!") ? value.slice(1) : undefined;
  return value;
}

/** The shell lines a command's options or configuration run (`COMMAND_OPTIONS`, `GIT_COMMAND_KEYS`). */
export function optionCommands(words: readonly string[], unknown: string): string[] {
  const program = (words[0] ?? "").split("/").pop() ?? "";
  const row = COMMAND_OPTIONS.find((candidate) => candidate.programs.test(program));
  if (!row) return [];
  const out: string[] = [];
  const args = words.slice(1);
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word === unknown) continue;
    for (const option of row.options ?? []) {
      if (word === option && args[i + 1] !== undefined && args[i + 1] !== unknown) out.push(args[i + 1]);
      else if (option.startsWith("--") && word.startsWith(`${option}=`)) out.push(word.slice(option.length + 1));
    }
    if (row.keyOption && (word === row.keyOption || (word.startsWith(row.keyOption) && word.length > row.keyOption.length && !word.startsWith("--")))) {
      const setting = word === row.keyOption ? args[i + 1] : word.slice(row.keyOption.length);
      if (setting === undefined || setting === unknown) continue;
      const eq = setting.indexOf("=");
      if (eq < 0) continue;
      const key = setting.slice(0, eq);
      const value = setting.slice(eq + 1);
      if (program === "git") {
        const line = gitCommandValue(key, value);
        if (line) out.push(line);
      } else if (row.keys?.test(key)) out.push(value);
    }
  }
  // `npm pkg set scripts.x='…'`: a script `npm run x` runs (round 5 step 2: configuration that is a command).
  if (/^(npm|pnpm)$/.test(program) && args[0] === "pkg" && args[1] === "set") {
    for (const word of args.slice(2)) {
      const match = /^scripts\.[^=]+=(.+)$/s.exec(word);
      if (match && word !== unknown) out.push(match[1]);
    }
  }
  // `git config [--global] key value`: a value that is a command.
  if (program === "git") {
    const at = args.findIndex((word) => !word.startsWith("-"));
    if (args[at] === "config") {
      const rest = args.slice(at + 1).filter((word) => !word.startsWith("-"));
      if (rest.length >= 2 && rest[0] !== unknown && rest[1] !== unknown) {
        const line = gitCommandValue(rest[0], rest[1]);
        if (line) out.push(line);
      }
    }
  }
  return out;
}
