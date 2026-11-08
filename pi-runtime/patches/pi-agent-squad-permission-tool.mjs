// Build-time patch: give pi-agent-squad's local subagents `request_permission`.
//
// Why this exists
// ---------------
// A subagent runs with a `--tools` allowlist (its profile's tools plus the
// messaging tools), and Pi drops every tool the allowlist does not name, even
// one an extension registers. Cypher's permission extension re-registers the
// file tools under their own names, so they survive, but its
// `request_permission` did not: a subagent's file-tool denial told it to call
// a tool it did not have. Wherever the allowlist names a file tool, the patch
// adds `request_permission` after the messaging tools. With permission control
// off nothing registers it and Pi ignores the name.
//
// The engine builds the allowlist of a Cypher-hosted child chat itself
// (`crates/harness/src/pi/spawn.rs`, the same rule); this covers the runs
// pi-agent-squad spawns itself: the fallback when Cypher cannot host one, and
// its agent pool.
//
// Lifetime
// --------
// Until pi-agent-squad lets a host add tools to every child. Pinned to the
// bundled version: a version bump must re-check the anchors
// (pi-agent-squad-permission-tool.staged.test.mjs fails on an allowlist this
// patch does not cover).
//
// Failure policy: hard. A missing anchor means pi-agent-squad changed shape.
//
// Usage: node pi-agent-squad-permission-tool.mjs <pi-agent-squad-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-agent-squad-permission-tool.mjs <pi-agent-squad-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: permission-tool";
const EXPECTED_VERSION = "0.9.0";
requireVersion(packageDir, { name: "pi-agent-squad", expected: EXPECTED_VERSION, script: "pi-agent-squad-permission-tool.mjs" });

/** The messaging loop over `name`, then `request_permission` beside a file tool. */
const edit = (label, name) => ({
  label,
  from: `		for (const ${name} of ["send_message", "read_inbox", "reply_message"]) {
			if (!tools.includes(${name})) tools.push(${name});
		}
`,
  to: `		for (const ${name} of ["send_message", "read_inbox", "reply_message"]) {
			if (!tools.includes(${name})) tools.push(${name});
		}
		// ${MARKER}: file-tool denials point at request_permission.
		if (tools.some((tool) => ["read", "write", "edit", "ls", "grep", "find"].includes(tool)) && !tools.includes("request_permission")) {
			tools.push("request_permission");
		}
`,
});

const FILES = {
  "spawn.ts": [edit("spawnInteractiveSubagent", "tool"), edit("spawnSubagent", "t")],
  "pool.ts": [edit("ensureProcess", "t")],
};

const patched = Object.entries(FILES)
  .filter(([file, edits]) => patchFile(packageDir, file, { name: "pi-agent-squad", marker: MARKER, edits }))
  .map(([file]) => file);
console.log(
  patched.length
    ? `pi-agent-squad patch: request_permission beside subagent file tools (${patched.join(", ")})`
    : "pi-agent-squad patch: permission tool already applied",
);
