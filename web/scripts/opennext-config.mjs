#!/usr/bin/env node
// OpenNext still reads the legacy config shape. Derive it with Cloudflare's
// converter, keeping cloudflare.config.ts as the only Worker configuration.
import { convertToWranglerConfig, loadAndParseConfig } from "@cloudflare/config";
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const { result } = await loadAndParseConfig(resolve("cloudflare.config.ts"), {
  isPreview: false,
  mode: undefined,
});
if (!result.success) throw new Error(`Invalid Cloudflare configuration: ${result.error}`);
const config = convertToWranglerConfig(result.data);
config.main = resolve(config.main);
config.assets = { ...config.assets, directory: resolve(".open-next/assets") };
mkdirSync(".cloudflare", { recursive: true });
writeFileSync(".cloudflare/opennext.json", `${JSON.stringify(config, null, 2)}\n`);
