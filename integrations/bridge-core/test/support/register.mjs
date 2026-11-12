import { register } from "node:module";

const stubs = {
  "@wecom/aibot-node-sdk": new URL("./sdk-stubs/wecom.mjs", import.meta.url).href,
  "@larksuiteoapi/node-sdk": new URL("./sdk-stubs/lark.mjs", import.meta.url).href
};

const hook = `
const stubs = ${JSON.stringify(stubs)};
export async function resolve(specifier, context, nextResolve) {
  if (stubs[specifier]) return { url: stubs[specifier], shortCircuit: true };
  return nextResolve(specifier, context);
}
`;

register(`data:text/javascript,${encodeURIComponent(hook)}`);
