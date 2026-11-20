#!/usr/bin/env node
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { REPO_ROOT } from "./facts-lib.mjs";
import { writeAgentDocs } from "./docs-sources-lib.mjs";

writeAgentDocs(resolve(dirname(fileURLToPath(import.meta.url)), ".."), REPO_ROOT);
