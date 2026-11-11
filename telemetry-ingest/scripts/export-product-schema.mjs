// Node 22.18+ can import this dependency-free TypeScript contract directly.
import { writeFileSync } from "node:fs";
import { CWC_PRODUCT_SCHEMA, WEBSITE_FUNNEL_SCHEMA } from "../src/schema.ts";

writeFileSync(
  new URL("../schema/cwc-product-v3.schema.json", import.meta.url),
  `${JSON.stringify(CWC_PRODUCT_SCHEMA, null, 2)}\n`,
);

writeFileSync(new URL("../schema/website-funnel-v5.schema.json", import.meta.url), `${JSON.stringify(WEBSITE_FUNNEL_SCHEMA, null, 2)}\n`);
