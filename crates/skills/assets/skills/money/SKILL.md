---
name: money
description: Bank accounts, balances, and transactions via Plaid. Use when: bank, balance, transactions, spending, accounts, or plaid.
invocation: model+user
---

# Money

## When to use
Account balances, transaction history, and spending analysis across the
user's bank accounts.

## Setup
Requires Plaid API credentials. Fail loud when they are missing:

```
PLAID_CLIENT_ID=... PLAID_SECRET=...  # sandbox keys first, production only when asked
```

The Link flow opens in the user's browser — the user completes it, the agent
drives everything after token exchange. Start in `sandbox`; touch
`production` only with explicit approval.

## Workflow
1. Exchange the public token once: `POST /item/public_token/exchange`.
2. Balances: `POST /accounts/balance`. History: `POST /transactions/sync`
   with its cursor, paging until `has_more` is false.
3. Report with dates, merchant names, and amounts; call out pending vs posted.
4. Spending analysis aggregates by category or merchant on request.

## Non-goals
- Do not move money, pay bills, or initiate transfers. Read-only, always.
- Do not store account or routing numbers beyond the session need.
- Do not paste secrets into chat, logs, or files.
