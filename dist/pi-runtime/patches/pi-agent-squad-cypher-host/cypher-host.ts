/**
 * CYPHER-RUNTIME-PATCH: cypher-host
 *
 * Host a pi-agent-squad subagent run as a Cypher child chat.
 *
 * Upstream, every `subagent` call spawns its own RPC child Pi process. Under
 * Cypher that child is invisible: the Subagents inspector gets a status row
 * but no session behind it, so there is nothing to open while it runs (or
 * after). The engine already hosts child chats — `StartSubagent` creates a
 * hidden, navigable child chat and queues its first run through the normal
 * session engine, `WatchAgentEvents` streams that chat's agent events, and
 * `QueueCommand` steers or interrupts it. This module drives that bridge and
 * answers with the same `SingleResult` / session-handle shapes the
 * extension's own spawn path produces, so the rest of the extension is
 * untouched.
 *
 * Installed by dist/pi-runtime/patches/pi-agent-squad-cypher-host.mjs, which
 * also routes `spawnInteractiveSubagent` here and adds `childChatId` to the
 * `cypher.subagents.v1` snapshot. Self-contained on purpose (node builtins
 * only) so it can be tested without a Pi install.
 */

import { pathToFileURL } from "node:url";

/** The bridge protocol this module speaks (`CYPHER_SUBAGENT_BRIDGE`). */
export const CYPHER_SUBAGENT_BRIDGE_VERSION = 2;

/** Engine-side bounds (`StartSubagent`); a larger value is refused, not cut. */
const TASK_LABEL_MAX_CHARS = 500;
const PROMPT_MAX_BYTES = 64 * 1024;
const START_TIMEOUT_MS = 30_000;
const COMMAND_TIMEOUT_MS = 10_000;
/** How long an interrupted child gets to report its terminal event. */
const TEARDOWN_CAP_MS = 3_000;
/** Re-subscriptions after the event stream drops before the run is lost. */
const MAX_RESUBSCRIBES = 3;
const RESUBSCRIBE_DELAY_MS = 500;

const TRANSPORT_ERRORS = new Set([
	"Engine IPC request timed out",
	"Engine IPC disconnected",
]);

export interface CypherHostOptions {
	mode: "sync" | "async";
	/** The parent tool call this run answers to. */
	toolCallId?: string;
	/** The child chat now backing the run (the inspector row becomes navigable). */
	onChildChat?: (childChatId: string) => void;
}

export interface EngineClient {
	call(method: string, params?: unknown, options?: { timeoutMs?: number }): Promise<any>;
	subscribe(method: string, params?: unknown, options?: { signal?: AbortSignal }): AsyncIterable<any>;
	close(): void;
}

/** Just the fields of the extension's spawn options this path reads. */
export interface HostedSpawnOptions {
	agent: { name: string; systemPrompt: string; tools?: string[]; model?: string; thinking?: string };
	task: string;
	address?: string;
	cwd?: string;
	messageRoot: string;
	runId: string;
	childIndex: number;
	signal?: AbortSignal;
	timeoutMs?: number;
	onStarted?: (childSessionFile?: string) => void;
	onSession?: (session: any) => void;
	onEvent?: (event: any) => void;
}

export interface HostedResult {
	agent: string;
	task: string;
	exitCode: number;
	messages: Array<{ role: string; content?: unknown; [k: string]: unknown }>;
	stderr: string;
	usage: {
		input: number;
		output: number;
		cacheRead: number;
		cacheWrite: number;
		cost: number;
		contextTokens: number;
		turns: number;
	};
	model?: string;
	stopReason?: string;
	errorMessage?: string;
}

export interface HostDependencies {
	env?: NodeJS.ProcessEnv;
	connect?: () => Promise<EngineClient>;
}

/**
 * Nothing was created on the engine: the caller may run the subagent the
 * extension's own way instead. Only thrown before a child chat exists (or
 * after the engine explicitly refused one), never once a run may be live.
 */
export class CypherHostUnavailable extends Error {
	constructor(message: string) {
		super(message);
		this.name = "CypherHostUnavailable";
	}
}

/** Is this Pi process a Cypher-hosted parent that can host its children? */
export function cypherHostAvailable(env: NodeJS.ProcessEnv = process.env): boolean {
	const version = Number.parseInt(env.CYPHER_SUBAGENT_BRIDGE ?? "", 10);
	return (
		Number.isInteger(version) &&
		version >= CYPHER_SUBAGENT_BRIDGE_VERSION &&
		Boolean(env.CYPHER_ENGINE_SOCKET) &&
		Boolean(env.CYPHER_CHAT_ID) &&
		Boolean(env.CYPHER_ENGINE_CLIENT_MODULE)
	);
}

async function connectFromEnv(env: NodeJS.ProcessEnv): Promise<EngineClient> {
	const module = await import(pathToFileURL(env.CYPHER_ENGINE_CLIENT_MODULE as string).href);
	return await module.connectEngine({ socketPath: env.CYPHER_ENGINE_SOCKET });
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/** The inspector label: first line, whitespace-collapsed, ≤500 chars. */
export function taskLabel(task: string): string {
	const text = String(task ?? "").replace(/\s+/g, " ").trim();
	const chars = [...text];
	return chars.length <= TASK_LABEL_MAX_CHARS
		? text
		: `${chars.slice(0, TASK_LABEL_MAX_CHARS - 1).join("")}…`;
}

function toolName(call: any): string {
	if (!call || typeof call !== "object") return "tool";
	if (typeof call.name === "string" && call.name) return call.name;
	if (typeof call.tool === "string" && call.tool) return call.tool;
	return typeof call.kind === "string" && call.kind ? call.kind : "tool";
}

function toolArgs(call: any): unknown {
	if (!call || typeof call !== "object") return undefined;
	return call.input && typeof call.input === "object" ? call.input : call;
}

function assistantMessage(text: string, model?: string): { role: string; content: unknown; model?: string } {
	return { role: "assistant", content: [{ type: "text", text }], ...(model ? { model } : {}) };
}

function emptyUsage(): HostedResult["usage"] {
	return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, contextTokens: 0, turns: 0 };
}

function withCap<T>(promise: Promise<T>, capMs: number): Promise<T | undefined> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	return Promise.race([
		promise,
		new Promise<undefined>((resolve) => {
			timer = setTimeout(() => resolve(undefined), capMs);
			timer.unref?.();
		}),
	]).finally(() => {
		if (timer) clearTimeout(timer);
	});
}

/**
 * Run one subagent as a Cypher child chat. Resolves with the extension's
 * `SingleResult` shape; rejects with `CypherHostUnavailable` when nothing was
 * started (fall back), or a plain Error for an aborted/lost run.
 */
export async function spawnCypherHostedSubagent(
	opts: HostedSpawnOptions,
	host: CypherHostOptions,
	deps: HostDependencies = {},
): Promise<HostedResult> {
	const env = deps.env ?? process.env;
	const { agent, task, runId, childIndex, signal } = opts;
	if (signal?.aborted) throw new Error("Subagent was aborted");
	const address = opts.address ?? agent.name;
	const timeoutMs = Math.max(1000, opts.timeoutMs ?? 6 * 60 * 60 * 1000);
	const timeoutMessage = `Subagent timed out after ${Math.round(timeoutMs / 1000)}s`;
	const prompt = String(task ?? "");
	if (Buffer.byteLength(prompt, "utf8") > PROMPT_MAX_BYTES) {
		throw new CypherHostUnavailable("task is larger than a Cypher child chat accepts");
	}

	let client: EngineClient;
	try {
		client = await (deps.connect ?? (() => connectFromEnv(env)))();
	} catch (error) {
		throw new CypherHostUnavailable(`Cypher engine unreachable: ${errorText(error)}`);
	}

	let childChatId: string;
	try {
		const label = taskLabel(prompt);
		const reply = await client.call(
			"StartSubagent",
			{
				parentChatId: env.CYPHER_CHAT_ID,
				runId,
				// The row shows the agent; the channel keys on the address.
				agent: agent.name,
				...(address !== agent.name ? { address } : {}),
				task: label,
				...(label !== prompt ? { prompt } : {}),
				mode: host.mode,
				...(host.toolCallId ? { toolCallId: host.toolCallId } : {}),
				...(opts.cwd ? { cwd: opts.cwd } : {}),
				systemPrompt: agent.systemPrompt ?? "",
				tools: agent.tools ?? [],
				...(agent.model ? { model: agent.model } : {}),
				...(agent.thinking ? { thinking: agent.thinking } : {}),
				messageRoot: opts.messageRoot,
				childIndex,
			},
			{ timeoutMs: START_TIMEOUT_MS },
		);
		if (typeof reply?.childChatId !== "string" || !reply.childChatId) {
			throw new Error("StartSubagent returned no childChatId");
		}
		childChatId = reply.childChatId;
	} catch (error) {
		const message = errorText(error);
		// A lost reply may still have queued the child: running it again
		// locally would do the work twice. Only an explicit refusal (the engine
		// rolls a failed start back) is safe to fall back from.
		if (TRANSPORT_ERRORS.has(message)) {
			client.close();
			throw new Error(`Cypher could not confirm the subagent start: ${message}`);
		}
		client.close();
		throw new CypherHostUnavailable(`Cypher refused the child chat: ${message}`);
	}

	const result: HostedResult = {
		agent: agent.name,
		task,
		exitCode: 0,
		messages: [],
		stderr: "",
		usage: emptyUsage(),
		model: agent.model,
	};

	// ---- one event pump; the run and routed messages both read it ---------
	type Terminal =
		| { kind: "done"; status: string; result?: string; error?: string }
		| { kind: "lost"; error: Error };
	let streaming = false;
	let started = false;
	let pendingText = "";
	let lastError: string | undefined;
	let terminal: Terminal | undefined;
	const listeners = new Set<(event: any) => void>();
	const turnWaiters = new Set<(outcome: Terminal) => void>();
	let resolveTerminal!: (outcome: Terminal) => void;
	const terminalPromise = new Promise<Terminal>((resolve) => {
		resolveTerminal = resolve;
	});
	let resolveStarted!: () => void;
	const startedPromise = new Promise<void>((resolve) => {
		resolveStarted = resolve;
	});

	const emit = (event: any) => {
		try {
			opts.onEvent?.(event);
		} catch {
			/* a status hook must never break the run */
		}
		for (const listener of listeners) {
			try {
				listener(event);
			} catch {
				/* one listener must not break the run */
			}
		}
	};
	const markStarted = () => {
		if (started) return;
		started = true;
		streaming = true;
		resolveStarted();
	};
	const flushMessage = () => {
		if (!pendingText) return;
		const message = assistantMessage(pendingText, result.model);
		pendingText = "";
		result.messages.push(message);
		result.usage.turns++;
		emit({ type: "message_end", message });
	};
	const settle = (outcome: Terminal) => {
		if (terminal) return;
		terminal = outcome;
		streaming = false;
		resolveStarted();
		for (const waiter of turnWaiters) waiter(outcome);
		turnWaiters.clear();
		resolveTerminal(outcome);
	};

	const dispatch = (event: any) => {
		switch (event?.type) {
			case "sessionStarted":
				if (typeof event.model === "string" && event.model) result.model = event.model;
				markStarted();
				return;
			case "textDelta":
				markStarted();
				if (typeof event.text === "string") pendingText += event.text;
				return;
			case "assistantMessageCompleted":
				flushMessage();
				return;
			case "toolCall":
				markStarted();
				flushMessage();
				emit({ type: "tool_execution_start", toolName: toolName(event.call), args: toolArgs(event.call) });
				return;
			case "error":
				if (typeof event.message === "string") lastError = event.message;
				return;
			case "done": {
				flushMessage();
				const outcome: Terminal = {
					kind: "done",
					status: String(event.status ?? "completed"),
					result: typeof event.result === "string" ? event.result : undefined,
					error: typeof event.error === "string" ? event.error : undefined,
				};
				emit({ type: "agent_settled" });
				settle(outcome);
				return;
			}
			default:
				return;
		}
	};

	const pumpAbort = new AbortController();
	const pump = (async () => {
		let resubscribes = 0;
		while (!terminal && !pumpAbort.signal.aborted) {
			try {
				for await (const event of client.subscribe(
					"WatchAgentEvents",
					{ chatId: childChatId },
					{ signal: pumpAbort.signal },
				)) {
					dispatch(event);
					if (terminal) return;
				}
			} catch (error) {
				if (pumpAbort.signal.aborted || terminal) return;
				lastError = errorText(error);
			}
			// A re-subscription replays the child's journal from the start:
			// drop the partial text so it is not counted twice.
			pendingText = "";
			result.messages = [];
			result.usage.turns = 0;
			if (++resubscribes > MAX_RESUBSCRIBES) {
				settle({
					kind: "lost",
					error: new Error(`Lost the Cypher child chat's event stream${lastError ? `: ${lastError}` : ""}`),
				});
				return;
			}
			await new Promise((resolve) => setTimeout(resolve, RESUBSCRIBE_DELAY_MS));
		}
	})();

	const queue = (command: Record<string, unknown>) =>
		client.call("QueueCommand", { chatId: childChatId, command }, { timeoutMs: COMMAND_TIMEOUT_MS });
	const interrupt = () => queue({ kind: "interrupt" });
	const nextTurn = () =>
		new Promise<Terminal>((resolve) => {
			if (terminal) resolve(terminal);
			else turnWaiters.add(resolve);
		});

	const session = {
		agent: address,
		sessionFile: undefined,
		childChatId,
		getMessages: async () => [...result.messages],
		isStreaming: async () => streaming,
		send: async (message: string) => {
			await startedPromise;
			if (terminal) throw new Error("Subagent session has ended.");
			await queue({ kind: "steer", prompt: message, messageId: null });
		},
		sendAndWait: async (message: string, messageTimeoutMs: number, messageSignal?: AbortSignal) => {
			await startedPromise;
			if (terminal) throw new Error("Subagent session has ended.");
			if (messageSignal?.aborted) throw new Error("Message delivery cancelled.");
			const before = result.messages.length;
			const turn = nextTurn();
			await queue({ kind: "steer", prompt: message, messageId: null });
			let timer: ReturnType<typeof setTimeout> | undefined;
			let onAbort: (() => void) | undefined;
			try {
				const outcome = await Promise.race([
					turn,
					new Promise<never>((_resolve, reject) => {
						timer = setTimeout(
							() => reject(new Error("Timeout waiting for agent to become idle.")),
							messageTimeoutMs,
						);
						timer.unref?.();
					}),
					new Promise<never>((_resolve, reject) => {
						onAbort = () => reject(new Error("Message delivery cancelled."));
						messageSignal?.addEventListener("abort", onAbort, { once: true });
					}),
				]);
				if (outcome.kind === "lost") throw outcome.error;
				const text = result.messages
					.slice(before)
					.map((m: any) => (Array.isArray(m.content) ? m.content.map((b: any) => b?.text ?? "").join("") : ""))
					.join("")
					.trim();
				return text || outcome.result || "";
			} finally {
				if (timer) clearTimeout(timer);
				if (onAbort) messageSignal?.removeEventListener("abort", onAbort);
			}
		},
		abort: async () => {
			await interrupt();
		},
		subscribe: (listener: (event: any) => void) => {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
	};

	let timeoutTimer: ReturnType<typeof setTimeout> | undefined;
	let onAbort: (() => void) | undefined;
	try {
		try {
			host.onChildChat?.(childChatId);
		} catch {
			/* linking the inspector row is best effort */
		}
		opts.onStarted?.(undefined);
		opts.onSession?.(session);

		type Stop = { kind: "timeout" } | { kind: "aborted" };
		const stopped = new Promise<Stop>((resolve) => {
			timeoutTimer = setTimeout(() => resolve({ kind: "timeout" }), timeoutMs);
			timeoutTimer.unref?.();
			onAbort = () => resolve({ kind: "aborted" });
			if (signal?.aborted) onAbort();
			else signal?.addEventListener("abort", onAbort, { once: true });
		});
		const first = await Promise.race([terminalPromise, stopped]);
		if (first.kind === "timeout" || first.kind === "aborted") {
			// The child chat outlives this call; stop its turn so a cancelled
			// task does not keep spending, then give it a moment to settle.
			await withCap(interrupt().catch(() => {}), TEARDOWN_CAP_MS);
			await withCap(terminalPromise, TEARDOWN_CAP_MS);
			if (first.kind === "aborted") throw new Error("Subagent was aborted");
			result.exitCode = 124;
			result.stopReason = "error";
			result.errorMessage = timeoutMessage;
			return result;
		}
		if (first.kind === "lost") throw first.error;

		// The engine's terminal event carries the child's final answer (its
		// last assistant message) — authoritative even if a lagging stream
		// skipped deltas; the streamed messages are only a fallback.
		if (first.result) {
			result.messages = [assistantMessage(first.result, result.model)];
			result.usage.turns = Math.max(1, result.usage.turns);
		}
		if (first.status === "errored") {
			result.stopReason = "error";
			result.errorMessage = first.error || lastError || "subagent reported an error";
		} else if (first.status === "interrupted") {
			// Stopped from its own chat in Cypher, not by the parent.
			result.exitCode = 130;
			result.stopReason = "aborted";
			result.errorMessage = "Subagent was aborted from its Cypher session";
		} else {
			result.stopReason = "stop";
		}
		return result;
	} finally {
		if (timeoutTimer) clearTimeout(timeoutTimer);
		if (onAbort) signal?.removeEventListener("abort", onAbort);
		pumpAbort.abort();
		await withCap(pump, TEARDOWN_CAP_MS);
		client.close();
		for (const listener of listeners) {
			try {
				listener({ type: "session_closed", error: terminal?.kind === "lost" ? terminal.error.message : undefined });
			} catch {
				/* ignore */
			}
		}
		listeners.clear();
	}
}
