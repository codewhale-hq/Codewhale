# Codewhale web copy style

Sources: UT Dallas JSOM Business Communication Center guides
(`refs/utd-business-communication`), `codewhale-design/DIRECTION.md` "Words",
`codewhale-ops/CURRENT_DECISIONS.md` §25b.

## Rules
1. **Lead with a verb or the reader's outcome.** "Records every change", not "Receipts for every change". Résumé-style: verb, object, proof.
2. **Know the reader.** A developer deciding whether to install. Say what they get, then the one next step.
3. **One idea per unit.** One heading, one claim, one CTA per block.
4. **Concrete over adjectives.** Name the command, the file, the mode. `codewhale exec` beats "scriptable".
5. **Never overclaim.** Every claim must be true in the released build today. Check `docs/features.toml` and `docs/*.md`; if unsure, write the narrower claim. Preview and development surfaces say so.
6. **Short.** Headlines ≤ 8 words, sentence case, no period unless it is two sentences. Body ≤ 2 short sentences.
7. **Parallel structure.** List items share a grammatical form (all verbs, or all nouns).
8. **"You", not "we" or "users".** Address the reader directly.
9. **Plain English.** Read it aloud; rewrite anything that doesn't parse on the first pass.
10. **Exact product vocabulary.** Plan / Work / Operate, Ask / Auto-Review / Full Access, Fleet, Runtime, `codewhale exec`, `/provider`, `/model`.
11. **No public pricing. No desktop download offer.** Placeholders (`{brand}`, `{version}`, `{tag}`) stay verbatim.

12. **Write whole sentences.** No short, choppy fragments or stacked two-word slogans ("Any model. Your machine."), in headlines or body. A heading may be a plain noun phrase; a slogan built from fragments is not. (Founder, 2026-11-10.)

## Slop blacklist
Words: seamless, powerful, unlock, leverage, empower, effortless, robust,
cutting-edge, journey, elevate, supercharge, revolutionize, next-generation,
world-class, best-in-class, game-changer, harness, delve, streamline, unleash,
first-class, simply, just, truly, really, very.

Patterns:
- Stacked hedges ("can help you potentially", "may be able to").
- "Whether you're X or Y…" openers.
- Rule-of-three padding: a third item added for rhythm, not content.
- Em-dash flourishes used for drama; use a period or colon.
- "Not just X, but Y."
- Vague tails: "and more", "and beyond", "everything you need".
- Throat-clearing: "Welcome to", "We're excited", "would love to hear from you".
- Generic headings: "What you can do with X", "Getting started with X", "Features".
