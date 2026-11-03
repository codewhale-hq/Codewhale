import type {
  RuntimeImageInput, RuntimeEventEnvelope, RuntimeCapabilities,
  RuntimeExperimentalCapabilities, DynamicToolSpec, DynamicToolSpecOutput,
  DynamicToolItemStatus, DynamicToolCallParams, DynamicToolCallResult,
  DynamicToolCallContent, TurnEnvironmentParams,
} from "../index.js";
import { createRuntimeClient } from "../index.js";
import type { Json } from "../generated/runtime.js";

type Assert<T extends true> = T;
type RootUndefinedIsRejected = Assert<undefined extends Json ? false : true>;
type ArrayUndefinedIsRejected = Assert<undefined[] extends Json ? false : true>;
type PublishedSpecRequiresDefault = Assert<{} extends Pick<DynamicToolSpecOutput, "defer_loading"> ? false : true>;

declare const image: RuntimeImageInput;
declare const capabilities: RuntimeCapabilities;
declare const experimental: RuntimeExperimentalCapabilities;
declare const publishedSpec: DynamicToolSpecOutput;
declare const status: DynamicToolItemStatus;
declare const content: DynamicToolCallContent;
declare const environment: TurnEnvironmentParams;

const inputSpec: DynamicToolSpec = { name: "fixture", description: "fixture", input_schema: {} };
const result: DynamicToolCallResult = { success: false };
const request: DynamicToolCallParams = { thread_id: "thread", turn_id: "turn", call_id: "call", tool: "fixture", arguments: {} };
const event: RuntimeEventEnvelope = {
  schema_version: 1, seq: 1, event: "tool_call.requested", kind: "tool_call.requested",
  thread_id: "thread", turn_id: "turn", item_id: null, timestamp: "now", payload: request,
};
const values: Json = [image, capabilities, experimental, inputSpec, publishedSpec, status, content, environment, result, request, event];
const optionalMembers: Json = { optional: undefined, nested: { omitted: undefined, values } };
void optionalMembers;

const settlement: Promise<void> = createRuntimeClient().deliverDynamicToolResult(request, result, { signal: new AbortController().signal });
void settlement;
