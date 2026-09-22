// Build-time patch: host pi-agent-squad's subagent runs as Cypher child chats.
//
// Why this exists
// ---------------
// pi-agent-squad runs every `subagent` call as its own RPC child Pi process
// (spawn.ts). Under Cypher that process is invisible: the Subagents inspector
// shows a status row with nothing behind it, so a running (or finished)
// subagent cannot be opened. The engine already hosts child chats through its
// StartSubagent / WatchAgentEvents / QueueCommand bridge — the extension just
// never calls it. This patch installs `cypher-host.ts`
// (./pi-agent-squad-cypher-host/) and routes the extension through it:
//
// - spawn.ts: `spawnInteractiveSubagent` hands a run to the Cypher host when
//   the caller asked for it and the engine advertises a bridge version this
//   module speaks; anything the host refuses before a child exists falls back
//   to the upstream process spawn unchanged.
// - index.ts: the sync and background `subagent` paths ask for hosting and
//   pass the parent tool call id.
// - cypher-status.ts: the `cypher.subagents.v1` snapshot carries each run's
//   `childChatId`, which is what makes the inspector row navigable.
//
// Lifetime
// --------
// Until pi-agent-squad grows a host hook of its own. Pinned to the bundled
// version: a version bump must re-check the anchors (and the behaviour).
//
// Failure policy: hard. A missing anchor means pi-agent-squad changed shape;
// silently shipping an unpatched bundle would bring back sessions that cannot
// be opened. The packaging script runs under `set -e`.
//
// Usage: node pi-agent-squad-cypher-host.mjs <pi-agent-squad-package-dir>

import { copyFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const [, , packageDir] = process.argv;
if (!packageDir) {
  console.error("usage: pi-agent-squad-cypher-host.mjs <pi-agent-squad-package-dir>");
  process.exit(1);
}

const MARKER = "CYPHER-RUNTIME-PATCH: cypher-host";
const EXPECTED_VERSION = "0.9.0";
const HOST_SOURCE = join(dirname(fileURLToPath(import.meta.url)), "pi-agent-squad-cypher-host", "cypher-host.ts");

const version = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf-8")).version;
if (version !== EXPECTED_VERSION) {
  console.error(
    `pi-agent-squad patch: expected ${EXPECTED_VERSION}, found ${version}.\n` +
      "Re-check dist/pi-runtime/patches/pi-agent-squad-cypher-host.mjs against the new version.",
  );
  process.exit(1);
}

const FILES = {
  "spawn.ts": [
    {
      label: "import",
      from: `import type { SubagentSessionHandle } from "./session.ts";\n`,
      to: `import type { SubagentSessionHandle } from "./session.ts";
// ${MARKER}
import {
	CypherHostUnavailable,
	cypherHostAvailable,
	spawnCypherHostedSubagent,
	type CypherHostOptions,
} from "./cypher-host.ts";
`,
    },
    {
      label: "InteractiveSpawnOptions",
      from: `	onEvent?: (event: any) => void;
}`,
      to: `	onEvent?: (event: any) => void;
	/** ${MARKER} — host this run as a Cypher child chat when the engine can. */
	cypherHost?: CypherHostOptions;
}`,
    },
    {
      label: "spawnInteractiveSubagent",
      from: `export async function spawnInteractiveSubagent(opts: InteractiveSpawnOptions): Promise<SingleResult> {
`,
      to: `export async function spawnInteractiveSubagent(opts: InteractiveSpawnOptions): Promise<SingleResult> {
	// ${MARKER} — a Cypher child chat instead of an invisible process.
	if (opts.cypherHost && cypherHostAvailable()) {
		try {
			ensureDir(channelDir(opts.messageRoot, opts.runId, opts.address ?? opts.agent.name, opts.childIndex));
			return (await spawnCypherHostedSubagent(opts, opts.cypherHost)) as SingleResult;
		} catch (error) {
			if (!(error instanceof CypherHostUnavailable)) throw error;
			process.stderr.write(\`[pi-agent-squad] \${error.message}; running the subagent as a local process\\n\`);
		}
	}
`,
    },
  ],
  "index.ts": [
    {
      label: "background spawn",
      from: `				timeoutMs: effectiveTimeoutMs,
				persistSession: true,
				onEvent: (event) => cypherStatus.observeChildEvent(runId, event),`,
      to: `				timeoutMs: effectiveTimeoutMs,
				persistSession: true,
				// ${MARKER}
				cypherHost: {
					mode: "async",
					toolCallId,
					onChildChat: (childChatId) => cypherStatus.linkChild(runId, childChatId),
				},
				onEvent: (event) => cypherStatus.observeChildEvent(runId, event),`,
    },
    {
      label: "sync spawn",
      from: `					signal: runController.signal,
					timeoutMs,
					onEvent: (event) => cypherStatus.observeChildEvent(runId, event),`,
      to: `					signal: runController.signal,
					timeoutMs,
					// ${MARKER}
					cypherHost: {
						mode: "sync",
						toolCallId,
						onChildChat: (childChatId) => cypherStatus.linkChild(runId, childChatId),
					},
					onEvent: (event) => cypherStatus.observeChildEvent(runId, event),`,
    },
  ],
  "cypher-status.ts": [
    {
      label: "CypherRun.childChatId",
      from: `	endedAt?: number;
}

export interface CypherStatusSnapshot {`,
      to: `	endedAt?: number;
	/** ${MARKER} — the Cypher child chat hosting this run (the inspector opens it). */
	childChatId?: string;
}

export interface CypherStatusSnapshot {`,
    },
    {
      label: "linkChild",
      from: `	/** Model discovered mid-run, or a new progress line. No-op for unknown runs. */`,
      to: `	/** ${MARKER} — link a run to the Cypher child chat hosting it. */
	linkChild(runId: string, childChatId: string): void {
		const run = this.runs.get(runId);
		// Cypher drops a whole snapshot over an over-long id; never send one.
		if (!run || !childChatId || childChatId.length > 256 || run.childChatId === childChatId) return;
		run.childChatId = childChatId;
		run.updatedAt = Date.now();
		this.publish(true);
	}

	/** Model discovered mid-run, or a new progress line. No-op for unknown runs. */`,
    },
  ],
};

const patched = [];
for (const [file, edits] of Object.entries(FILES)) {
  const target = join(packageDir, file);
  if (!existsSync(target)) {
    console.error(`pi-agent-squad patch: ${target} not found`);
    process.exit(1);
  }
  const source = readFileSync(target, "utf-8");
  if (source.includes(MARKER)) continue;
  let out = source;
  for (const { label, from, to } of edits) {
    const at = out.indexOf(from);
    if (at < 0 || out.indexOf(from, at + 1) >= 0) {
      console.error(
        `pi-agent-squad patch: anchor ${at < 0 ? "not found" : "not unique"} (${file}: ${label}).\n` +
          "pi-agent-squad changed shape — update dist/pi-runtime/patches/ before packaging.",
      );
      process.exit(1);
    }
    out = out.replace(from, to);
  }
  writeFileSync(target, out);
  patched.push(file);
}
copyFileSync(HOST_SOURCE, join(packageDir, "cypher-host.ts"));
console.log(
  patched.length
    ? `pi-agent-squad patch: Cypher-hosted child chats (${patched.join(", ")})`
    : "pi-agent-squad patch: already applied",
);
