import { bindings, defineConfig, exports, triggers } from "cf/config";

export default defineConfig({
	worker: {
		name: "codewhale-web",
		compatibilityDate: "2025-04-01",
		compatibilityFlags: [
			"nodejs_compat",
			"global_fetch_strictly_public",
		],
		entrypoint: "worker.ts",
		observability: {
			enabled: true,
		},
		domains: [
			"codewhale.net",
			"www.codewhale.net",
		],
		triggers: [
			triggers.scheduled({
				schedule: "0 */6 * * *",
			}),
			triggers.scheduled({
				schedule: "0 0 * * *",
			}),
		],
		env: {
			GITHUB_REPO: bindings.text("codewhale-hq/CodeWhale"),
			DEEPSEEK_MODEL: bindings.text("deepseek-flash"),
			DEEPSEEK_BASE_URL: bindings.text("https://gateway.ai.cloudflare.com/v1/cf50f793171d7cb3b2ce23368b69cdcb/codewhale-web/deepseek"),
			SUPABASE_URL: bindings.text("https://mungbvkvpkxbkjzspehg.supabase.co"),
			CURATED_KV: bindings.kv({
				id: "abaa6a753c9d45bfa5c0afaf26dc67b3",
			}),
			NEXT_INC_CACHE_KV: bindings.kv({
				id: "a2e6f324db9b4b03bbc940a4ba246985",
			}),
			WORKER_SELF_REFERENCE: bindings.worker({
				worker: "codewhale-web",
			}),
			DRAFT_CLAIM_LOCK: bindings.durableObject({
				worker: "codewhale-web",
				exportName: "DraftClaimLock",
			}),
			ADMIN_LOGIN_LIMITER: bindings.rateLimit({
				namespace: "913001",
				simple: {
					limit: 5,
					period: 60,
				},
			}),
			MERCH_INTEREST_LIMITER: bindings.rateLimit({
				namespace: "913002",
				simple: {
					limit: 5,
					period: 60,
				},
			}),
			ASSETS: bindings.assets(),
		},
		// Same live class and SQLite storage as the former v1 migration.
		exports: { DraftClaimLock: exports.durableObject({ storage: "sqlite" }) },
	},
});
