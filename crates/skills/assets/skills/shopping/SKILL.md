---
name: shopping
description: Product research — compare options, prices, and reviews. Use when: buy, shopping, product recommendation, compare prices, best X to buy, or is this a good deal.
invocation: model+user
---

# Shopping

## When to use
Helping the user choose what to buy: shortlists, comparisons, price checks,
and deal evaluation.

## Setup
None — web research only. Fail loud when the request needs a logged-in
store account (order history, cart).

## Workflow
1. Pin the budget, must-have features, and deal-breakers before researching.
2. Research with web search: reviews, roundups, and retailer listings.
3. Present a comparison table (option, price, key specs, standout review
   point), then one recommendation with its tradeoff.
4. For deal questions, check price history where available (e.g. camelcamelcamel
   for Amazon) and say when history is unavailable.

## Non-goals
- Do not purchase anything or enter payment details. Ever.
- Do not add to carts or create store accounts.
- Do not present sponsored results as editorial picks.
