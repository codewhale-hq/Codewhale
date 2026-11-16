/**
 * `@deepseek-ai/dsh-sandbox` escalation excerpt for Codewhale built-in creator.
 *
 * Lines between the VERBATIM markers are copied unchanged from
 * `@deepseek-ai/dsh-sandbox@0.2.2-rc.1` `lib/index.js` (lines 9-42, 68-71, and
 * 137-158), MIT License, Copyright (c) 2026 DeepSeek. See ../LICENSE.
 */
import { assertNever } from "@deepseek-ai/dsh-util-values";

// ---- VERBATIM @deepseek-ai/dsh-sandbox@0.2.2-rc.1 lib/index.js:9-42 ----
/** English escalation messages. */
const en = { escalation: (mode, justification) => `Allow this operation with ${mode} permissions: ${justification}` };
/** Chinese escalation messages. */
const zh = { escalation: (mode, justification) => `允许本次操作使用 ${mode} 权限：${justification}` };
/** Japanese escalation messages. */
const ja = { escalation: (mode, justification) => `${mode} 権限でこの操作を許可します：${justification}` };
/** Spanish dictionary, checked against the English keys. */
const es = { escalation: (mode, justification) => `Permitir esta operación con permisos ${mode}: ${justification}` };
/** French dictionary, checked against the English keys. */
const fr = { escalation: (mode, justification) => `Autoriser cette opération avec les permissions ${mode} : ${justification}` };
/** German dictionary, checked against the English keys. */
const de = { escalation: (mode, justification) => `Diesen Vorgang mit ${mode}-Berechtigungen zulassen: ${justification}` };
/** Russian dictionary, checked against the English keys. */
const ru = { escalation: (mode, justification) => `Разрешить эту операцию с правами ${mode}: ${justification}` };
/** Portuguese dictionary, checked against the English keys. */
const pt = { escalation: (mode, justification) => `Permitir esta operação com permissões ${mode}: ${justification}` };
/**
* Build translations for the existing Client localized-text resolver.
* @param mode - requested sandbox mode.
* @param justification - raw model justification, preserved in every translation.
* @returns localized approval text.
*/
function escalationDisplayReason(mode, justification) {
	return {
		en: en.escalation(mode, justification),
		zh: zh.escalation(mode, justification),
		ja: ja.escalation(mode, justification),
		es: es.escalation(mode, justification),
		fr: fr.escalation(mode, justification),
		de: de.escalation(mode, justification),
		ru: ru.escalation(mode, justification),
		pt: pt.escalation(mode, justification)
	};
}
// ---- END VERBATIM ----

// ---- VERBATIM @deepseek-ai/dsh-sandbox@0.2.2-rc.1 lib/index.js:68-71 ----
const WIDER_MODES = {
	"read-only": ["workspace-write", "danger-full-access"],
	"workspace-write": ["danger-full-access"]
};
// ---- END VERBATIM ----

// ---- VERBATIM @deepseek-ai/dsh-sandbox@0.2.2-rc.1 lib/index.js:137-158 ----
async function approveEscalation(request, approval) {
	const { requestedMode: mode, effectiveMode, justification, subject } = request;
	if (mode === effectiveMode) return effectiveMode;
	if (!(WIDER_MODES[effectiveMode] ?? []).includes(mode)) throw new Error(`sandbox escalation to "${mode}" is not strictly wider than this call's current "${effectiveMode}" mode`);
	if (approval.approver === void 0) throw new Error(`sandbox escalation to "${mode}" requires approval, but no approval service is composed`);
	if (approval.agent === void 0) throw new Error(`sandbox escalation to "${mode}" requires approval, but the call has no agent to route it through`);
	const outcome = await approval.approver.request({
		agent: approval.agent,
		toolName: approval.toolName,
		callId: approval.callId,
		reason: `escalate sandbox to ${mode}: ${justification}`,
		displayReason: escalationDisplayReason(mode, justification),
		...approval.signal ? { signal: approval.signal } : {}
	});
	switch (outcome) {
		case "allowed-once": return mode;
		case "rejected": throw new Error(`the user rejected escalating this ${subject} to "${mode}"; it stays denied, so stop and explain instead of working around it`);
		case "cancelled": throw new Error(`approval for escalating to "${mode}" was cancelled`);
		case "unavailable": throw new Error(`sandbox escalation to "${mode}" requires approval, but no approval channel is available`);
		default: return assertNever(outcome, "EscalationOutcome");
	}
}
// ---- END VERBATIM ----

export { WIDER_MODES, escalationDisplayReason, approveEscalation };
