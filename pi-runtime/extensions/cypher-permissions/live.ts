/**
 * Live approvals on Linux. Landstrip's seccomp broker can hold a denied
 * syscall and ask over a socket (`--trap-fd`): one JSON line per query, one
 * `{"query_id","action"}` line back. The command waits while the user (or the
 * judge) decides, so an approval needs no rerun. macOS Seatbelt has no broker;
 * there bash elevation stays request-then-retry.
 *
 * Pi's bash executor spawns with three stdio streams only, so this module
 * replaces its `exec` for sandboxed commands with the same contract: stream
 * output, kill the whole process group on abort or timeout (`aborted`,
 * `timeout:<seconds>` errors), and report a signal-killed shell as 128 + the
 * signal number.
 */

import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { createServer, Socket } from "node:net";
import type { AddressInfo } from "node:net";
import { constants as osConstants } from "node:os";
import { StringDecoder } from "node:string_decoder";

export interface TrapQuery {
  kind: "filesystem" | "network";
  /** `read`/`write`, or `connect`/`bind`. */
  operation: string;
  /** The resolved path, or `address:port`. */
  target: string;
  queryId: string;
}

export function liveApprovalsSupported(): boolean {
  return process.platform === "linux";
}

/** One trap line, if it is a pending query. */
export function parseQuery(line: string): TrapQuery | undefined {
  let event: { kind?: string; state?: string; query_id?: string; operation?: string; path?: string; target?: string };
  try {
    event = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (event.state !== "query" || typeof event.query_id !== "string") return undefined;
  if (event.kind === "filesystem" && typeof event.path === "string") {
    return { kind: "filesystem", operation: event.operation ?? "read", target: event.path, queryId: event.query_id };
  }
  if (event.kind === "network" && typeof event.target === "string") {
    return { kind: "network", operation: event.operation ?? "connect", target: event.target, queryId: event.query_id };
  }
  return undefined;
}

export function responseLine(queryId: string, allow: boolean): string {
  return `${JSON.stringify({ query_id: queryId, action: allow ? "allow" : "deny" })}\n`;
}

/** A connected pair of sockets: one end for us, one for the child's fd 3. */
function socketPair(): Promise<[Socket, Socket]> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    let client: Socket | undefined;
    server.on("connection", (serverEnd) => {
      server.close();
      if (client) resolve([client, serverEnd]);
    });
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as AddressInfo;
      client = new Socket();
      client.on("error", reject);
      client.connect(port, "127.0.0.1");
    });
  });
}

const EXIT_STDIO_GRACE_MS = 250;

function waitForExit(child: ChildProcess): Promise<number | null> {
  return new Promise((resolve, reject) => {
    let exited = false;
    let code: number | null = null;
    let stdoutEnded = child.stdout === null;
    let stderrEnded = child.stderr === null;
    let timer: NodeJS.Timeout | undefined;
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      // A backgrounded grandchild may hold the pipes open forever.
      child.stdout?.destroy();
      child.stderr?.destroy();
      resolve(code);
    };
    const maybeFinish = () => {
      if (!exited) return;
      if (stdoutEnded && stderrEnded) finish();
      else timer ??= setTimeout(finish, EXIT_STDIO_GRACE_MS);
    };
    child.stdout?.once("end", () => {
      stdoutEnded = true;
      maybeFinish();
    });
    child.stderr?.once("end", () => {
      stderrEnded = true;
      maybeFinish();
    });
    child.once("exit", (exitCode) => {
      exited = true;
      code = exitCode;
      maybeFinish();
    });
    child.once("error", (error) => {
      if (!settled) {
        settled = true;
        reject(error);
      }
    });
  });
}

type ExecOptions = {
  onData: (data: Buffer) => void;
  signal?: AbortSignal;
  timeout?: number;
  env?: NodeJS.ProcessEnv;
};

/**
 * Pi's `BashOperations.exec`, running the command inside Landstrip with a
 * trap socket whose queries `answer` decides.
 */
export function trappedExec(
  binary: string,
  policyPath: string,
  shell: string,
  answer: (query: TrapQuery) => Promise<boolean>,
): (command: string, cwd: string, options: ExecOptions) => Promise<{ exitCode: number | null }> {
  return async (command, cwd, { onData, signal, timeout, env }) => {
    if (timeout !== undefined && (!Number.isFinite(timeout) || timeout <= 0)) {
      throw new Error("Invalid timeout: must be a finite number of seconds");
    }
    if (signal?.aborted) throw new Error("aborted");
    const [trapSocket, childEnd] = await socketPair();
    const child = spawn(binary, ["run", "--trap-fd", "3", "-p", policyPath, "--", shell, "-c", command], {
      cwd,
      env,
      detached: true,
      stdio: ["ignore", "pipe", "pipe", childEnd],
    });
    // The child holds its own copy of the descriptor.
    childEnd.destroy();

    const decoder = new StringDecoder("utf8");
    let buffer = "";
    let answers = Promise.resolve();
    trapSocket.on("error", () => {});
    trapSocket.on("data", (data: Buffer) => {
      buffer += decoder.write(data);
      let newline = buffer.indexOf("\n");
      while (newline >= 0) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        newline = buffer.indexOf("\n");
        const query = parseQuery(line);
        if (!query) continue;
        // One question at a time, in the order the syscalls arrived.
        answers = answers.then(async () => {
          const allow = await answer(query).catch(() => false);
          if (!trapSocket.destroyed) trapSocket.write(responseLine(query.queryId, allow));
        });
      }
    });

    const kill = () => {
      if (!child.pid) return;
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        try {
          child.kill("SIGKILL");
        } catch {
          // Already gone.
        }
      }
    };
    let timedOut = false;
    const timer = timeout !== undefined ? setTimeout(() => ((timedOut = true), kill()), timeout * 1000) : undefined;
    signal?.addEventListener("abort", kill, { once: true });
    child.stdout?.on("data", onData);
    child.stderr?.on("data", onData);
    try {
      const exitCode = await waitForExit(child);
      if (signal?.aborted) throw new Error("aborted");
      if (timedOut) throw new Error(`timeout:${timeout}`);
      const signalCode = child.signalCode;
      return {
        exitCode:
          exitCode ?? (signalCode ? 128 + ((osConstants.signals as Record<string, number>)[signalCode] ?? 0) : 1),
      };
    } finally {
      if (timer) clearTimeout(timer);
      signal?.removeEventListener("abort", kill);
      trapSocket.destroy();
    }
  };
}
