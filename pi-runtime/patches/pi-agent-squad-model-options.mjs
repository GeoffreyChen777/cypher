// Build-time patch: let the caller pick a subagent run's model and thinking.
//
// Why this exists
// ---------------
// pi-agent-squad reads a subagent's model and thinking level only from the
// agent's frontmatter (or `/subagent-config`, which rewrites that file for
// every later run). The orchestrating model could not choose either for one
// task — a cheap model for a quick lookup, a stronger one or more thinking
// for a hard review. The `subagent` tool now takes optional `model` and
// `thinking` and launches the run with a copy of the agent carrying them, so
// every launch path (Cypher-hosted child chat, local RPC process, the inspector
// row's model) picks them up unchanged.
//
// - model: `provider/id` from the session's available models; a bare id is
//   accepted when exactly one provider offers it. Anything else is refused
//   with the available ids, before a run starts.
// - thinking: one of `pi --thinking`'s levels. Cypher's harness drops
//   `--thinking` when no model is named, so a thinking-only override runs on
//   the agent's configured model, else the caller's current model.
//
// Lifetime
// --------
// Until pi-agent-squad takes per-run model options itself. Pinned to the
// bundled version: a version bump must re-check the anchors (and the
// behaviour — pi-agent-squad-model-options.staged.test.mjs runs the tool).
//
// Failure policy: hard. A missing anchor means pi-agent-squad changed shape;
// shipping without it would silently ignore the caller's model choice. The
// packaging script runs under `set -e`.
//
// Usage: node pi-agent-squad-model-options.mjs <pi-agent-squad-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-agent-squad-model-options.mjs <pi-agent-squad-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: model-options";
const EXPECTED_VERSION = "0.9.0";
requireVersion(packageDir, { name: "pi-agent-squad", expected: EXPECTED_VERSION, script: "pi-agent-squad-model-options.mjs" });

const EDITS = [
  {
    label: "launch profile helpers",
    from: `/** Update a \`model\` or \`thinking\` line in an agent's frontmatter */`,
    to: `// ${MARKER}
/** The levels \`pi --thinking\` accepts. */
const SUBAGENT_THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
/** How many model ids a refused \`model\` lists back. */
const MODEL_CHOICES_LISTED = 40;

type ModelContext = Parameters<typeof availableModelIds>[0] & { model?: { provider?: string; id?: string } };

/** Every \`provider/id\` this session could launch: the scoped models and the authenticated registry. */
function launchableModelIds(ctx: ModelContext): string[] {
	const ids = new Set(availableModelIds(ctx));
	for (const m of ctx.modelRegistry?.getAvailable?.() ?? []) {
		if (m.provider && m.id) ids.add(\`\${m.provider}/\${m.id}\`);
	}
	return [...ids];
}

function listModelIds(ids: readonly string[]): string {
	if (ids.length === 0) return "none";
	const shown = ids.slice(0, MODEL_CHOICES_LISTED).join(", ");
	return ids.length > MODEL_CHOICES_LISTED ? \`\${shown}, … (\${ids.length - MODEL_CHOICES_LISTED} more)\` : shown;
}

/** A requested model as the \`provider/id\` it names, or why it names none. */
function resolveRequestedModel(ctx: ModelContext, requested: string): { model: string } | { error: string } {
	const known = launchableModelIds(ctx);
	if (known.length === 0) {
		// No registry to check against: take a full id as given.
		return /^[^/\\s]+\\/\\S+$/.test(requested)
			? { model: requested }
			: { error: \`Model "\${requested}" must be given as provider/model.\` };
	}
	const wanted = requested.toLowerCase();
	const exact = known.find((id) => id === requested) ?? known.find((id) => id.toLowerCase() === wanted);
	if (exact) return { model: exact };
	const byId = known.filter((id) => id.slice(id.indexOf("/") + 1).toLowerCase() === wanted);
	if (byId.length === 1) return { model: byId[0]! };
	if (byId.length > 1) {
		return { error: \`Model "\${requested}" is offered by several providers; use one of: \${byId.join(", ")}\` };
	}
	return { error: \`Unknown model "\${requested}". Available: \${listModelIds(known)}\` };
}

/**
 * The agent a run launches with: the definition, with the caller's model and
 * thinking in place of its own. Without either it is the definition itself.
 */
function resolveLaunchAgent(
	agent: AgentConfig,
	params: { model?: string; thinking?: string },
	ctx: ModelContext,
): { agent: AgentConfig } | { error: string } {
	const requestedModel = params.model?.trim();
	const requestedThinking = params.thinking?.trim().toLowerCase();
	if (!requestedModel && !requestedThinking) return { agent };
	let model = agent.model;
	if (requestedModel) {
		const resolved = resolveRequestedModel(ctx, requestedModel);
		if ("error" in resolved) return resolved;
		model = resolved.model;
	}
	let thinking = agent.thinking;
	if (requestedThinking) {
		if (!(SUBAGENT_THINKING_LEVELS as readonly string[]).includes(requestedThinking)) {
			return {
				error: \`Unknown thinking level "\${params.thinking}". Use one of: \${SUBAGENT_THINKING_LEVELS.join(", ")}\`,
			};
		}
		thinking = requestedThinking;
		// Cypher launches \`--thinking\` only beside a model: keep the caller's.
		if (!model && ctx.model?.provider && ctx.model?.id) model = \`\${ctx.model.provider}/\${ctx.model.id}\`;
	}
	return { agent: { ...agent, model, thinking } };
}

/** Update a \`model\` or \`thinking\` line in an agent's frontmatter */`,
  },
  {
    label: "tool description",
    from: `			\`timeoutSeconds: optional; omit unless the user explicitly requested a time. Default \${DEFAULT_SUBAGENT_TIMEOUT_SECONDS}s (6 hours), range \${MIN_SUBAGENT_TIMEOUT_SECONDS}-\${MAX_SUBAGENT_TIMEOUT_SECONDS};\`,
`,
    to: `			\`timeoutSeconds: optional; omit unless the user explicitly requested a time. Default \${DEFAULT_SUBAGENT_TIMEOUT_SECONDS}s (6 hours), range \${MIN_SUBAGENT_TIMEOUT_SECONDS}-\${MAX_SUBAGENT_TIMEOUT_SECONDS};\`,
			// ${MARKER}
			"model / thinking: optional, for this run only; they replace the agent's configured model (provider/model) and thinking level. Omit to keep the agent's own;",
`,
  },
  {
    label: "tool parameters",
    from: `					description: \`Task timeout in seconds. Omit unless the user explicitly requested a time; default \${DEFAULT_SUBAGENT_TIMEOUT_SECONDS}s (6 hours).\`,
				}),
			),
		}),`,
    to: `					description: \`Task timeout in seconds. Omit unless the user explicitly requested a time; default \${DEFAULT_SUBAGENT_TIMEOUT_SECONDS}s (6 hours).\`,
				}),
			),
			// ${MARKER}
			model: Type.Optional(
				Type.String({
					maxLength: 200,
					description:
						"Model for this run as provider/model (for example anthropic/claude-sonnet-5-5); a bare model id works when one provider offers it. Omit to use the agent's configured model.",
				}),
			),
			thinking: Type.Optional(
				Type.Unsafe<(typeof SUBAGENT_THINKING_LEVELS)[number]>({
					type: "string",
					enum: [...SUBAGENT_THINKING_LEVELS],
					description: "Thinking level for this run. Omit to use the agent's configured level.",
				}),
			),
		}),`,
  },
  {
    label: "execute launch agent",
    from: `			const agents = discoverAgents(ctx.cwd);
			const agent = agents.find((a) => a.name === params.agent);
			if (!agent) {
				const available = agents.map((a) => a.name).join(", ") || "none";
				return {
					content: [{ type: "text", text: \`Unknown subagent "\${params.agent}". Available: \${available}\` }],
					details: undefined,
				};
			}
			const timeoutSeconds = Math.min(`,
    to: `			const agents = discoverAgents(ctx.cwd);
			const defined = agents.find((a) => a.name === params.agent);
			if (!defined) {
				const available = agents.map((a) => a.name).join(", ") || "none";
				return {
					content: [{ type: "text", text: \`Unknown subagent "\${params.agent}". Available: \${available}\` }],
					details: undefined,
				};
			}
			// ${MARKER} — the caller's model / thinking for this run only.
			const launch = resolveLaunchAgent(defined, params, ctx);
			if ("error" in launch) {
				return { content: [{ type: "text", text: launch.error }], details: undefined };
			}
			const agent = launch.agent;
			const timeoutSeconds = Math.min(`,
  },
];

if (!patchFile(packageDir, "index.ts", { name: "pi-agent-squad", marker: MARKER, edits: EDITS })) {
  console.log("pi-agent-squad patch: model options already applied");
  process.exit(0);
}
console.log("pi-agent-squad patch: per-run subagent model and thinking (index.ts)");
