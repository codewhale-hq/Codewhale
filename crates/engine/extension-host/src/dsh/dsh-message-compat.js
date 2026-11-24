import { deepFreeze } from '@deepseek-ai/dsh-util-values'

function brandString(value) {
	return value;
}

function randomUUID() {
	const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
	const hex = Array.from(bytes, (byte, index) => {
		return (index === 6 ? byte & 15 | 64 : index === 8 ? byte & 63 | 128 : byte).toString(16).padStart(2, "0");
	}).join("");
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function createMessage(input) {
	return deepFreeze(structuredClone({
		...input,
		id: brandString(randomUUID())
	}));
}

function createUserMessage(input) {
	return createMessage({
		...input,
		role: "user"
	});
}

function createAssistantMessage(input) {
	return createMessage({
		...input,
		role: "assistant"
	});
}

function createSystemMessage(input) {
	return createMessage({
		...input,
		role: "system"
	});
}

function createToolResultMessage(input) {
	return createMessage({
		...input,
		role: "tool"
	});
}

function freezeMessage(message) {
	return deepFreeze(structuredClone(message));
}

function contentHasImage(content) {
	if (!Array.isArray(content)) return false;
	return content.some((block) => block !== null && typeof block === "object" && (block.type === "image" || block.type === "image_ref"));
}

function ReasoningEffortId(id) {
	return brandString(id);
}

function MessageId(id) {
	return brandString(id);
}

function ToolCallId(id) {
	return brandString(id);
}

function isAgentLoopRequest(request) {
	return Boolean(request && typeof request === "object" && Array.isArray(request.messages));
}

function resolveRetryPolicy(policy) {
	const base = policy && typeof policy === "object" ? policy : {};
	return Object.freeze({
		maxRetries: typeof base.maxRetries === "number" && Number.isInteger(base.maxRetries) && base.maxRetries >= 0 ? base.maxRetries : 3,
		initialDelayMs: typeof base.initialDelayMs === "number" && base.initialDelayMs >= 0 ? base.initialDelayMs : 500,
		maxDelayMs: typeof base.maxDelayMs === "number" && base.maxDelayMs >= 0 ? base.maxDelayMs : 15000,
	});
}

function boundContextSummary(summary, maxChars = 4000) {
	if (typeof summary !== "string") return "";
	if (summary.length <= maxChars) return summary;
	return `${summary.slice(0, Math.max(0, maxChars - 1))}…`;
}

class BlockAssembler {
	constructor() {
		this._blocks = [];
	}
	push(delta) {
		if (!delta || typeof delta !== "object") return;
		if (delta.type === "text_delta" && typeof delta.text === "string") {
			const last = this._blocks[this._blocks.length - 1];
			if (last && last.type === "text") {
				last.text += delta.text;
			} else {
				this._blocks.push({ type: "text", text: delta.text });
			}
			return;
		}
		if (delta.type === "thinking_delta" && typeof delta.thinking === "string") {
			const last = this._blocks[this._blocks.length - 1];
			if (last && last.type === "thinking") {
				last.thinking += delta.thinking;
			} else {
				this._blocks.push({ type: "thinking", thinking: delta.thinking });
			}
			return;
		}
		if (delta.type === "text" || delta.type === "thinking" || delta.type === "tool_use" || delta.type === "image") {
			this._blocks.push(structuredClone(delta));
		}
	}
	snapshot() {
		return structuredClone(this._blocks);
	}
	finish() {
		return deepFreeze(structuredClone(this._blocks));
	}
	reset() {
		this._blocks = [];
	}
}

class LlmAdapter {
	constructor(name = "adapter") {
		this.name = name;
	}
}

function SessionId(id) {
	return brandString(id);
}

const SESSION_FORMAT_VERSION = 1;

const KNOWN_SESSION_EVENT_TYPES = new Set([
	"session/start",
	"session/end",
	"session/metadata",
	"turn/start",
	"turn/end",
	"message/user",
	"message/assistant",
	"message/system",
	"message/tool",
	"tool/call",
	"tool/result",
	"compaction",
	"error",
	"subagent/start",
	"subagent/end",
]);

function deriveEventMessage(event) {
	if (!event || typeof event !== "object") return undefined;
	if (event.message && typeof event.message === "object") {
		return event.message;
	}
	if (event.payload && typeof event.payload === "object" && event.payload.message && typeof event.payload.message === "object") {
		return event.payload.message;
	}
	const type = event.type;
	if (type === "message/user" || type === "message/assistant" || type === "message/system" || type === "message/tool") {
		const role = type.slice("message/".length);
		const content = Array.isArray(event.content)
			? event.content
			: Array.isArray(event.payload?.content)
				? event.payload.content
				: typeof event.text === "string"
					? [{ type: "text", text: event.text }]
					: [];
		return deepFreeze(structuredClone({
			id: typeof event.id === "string" ? event.id : brandString(randomUUID()),
			role,
			content,
		}));
	}
	return undefined;
}

const PERSONA_PREFIX_SECTION = "deployment:persona-prefix";

var HarnessError = class extends Error {
	code;
	constructor(message, code, options) {
		super(message, options);
		this.code = code;
		this.name = new.target.name;
	}
};

var LlmError = class extends HarnessError {
	constructor(message, code, options) {
		super(message, code, options);
		this.name = "LlmError";
	}
};

var UserQuestionError = class extends HarnessError {
	constructor(message, code, options) {
		super(message, code, options);
		this.name = "UserQuestionError";
	}
};

Object.defineProperty(HarnessError, "name", { value: "HarnessError" });
Object.defineProperty(LlmError, "name", { value: "LlmError" });
Object.defineProperty(UserQuestionError, "name", { value: "UserQuestionError" });

export {
	createUserMessage,
	createAssistantMessage,
	createSystemMessage,
	createToolResultMessage,
	freezeMessage,
	contentHasImage,
	ReasoningEffortId,
	MessageId,
	ToolCallId,
	isAgentLoopRequest,
	resolveRetryPolicy,
	boundContextSummary,
	BlockAssembler,
	LlmAdapter,
	LlmError,
	SessionId,
	SESSION_FORMAT_VERSION,
	KNOWN_SESSION_EVENT_TYPES,
	deriveEventMessage,
	PERSONA_PREFIX_SECTION,
	HarnessError,
	UserQuestionError,
}
