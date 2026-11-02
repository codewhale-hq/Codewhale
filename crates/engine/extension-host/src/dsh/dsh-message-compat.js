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

function SessionId(id) {
	return brandString(id);
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

var UserQuestionError = class extends HarnessError {
	constructor(message, code, options) {
		super(message, code, options);
		this.name = "UserQuestionError";
	}
};

Object.defineProperty(HarnessError, "name", { value: "HarnessError" });
Object.defineProperty(UserQuestionError, "name", { value: "UserQuestionError" });

export { createUserMessage, SessionId, PERSONA_PREFIX_SECTION, HarnessError, UserQuestionError }
