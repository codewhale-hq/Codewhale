---
name: forget
description: Propose forgetting stored facts; the user confirms in Context Lens. Use when: forget, don't remember that, delete a memory, drop that fact, or stop knowing something.
invocation: model+user
---

# Forget

## When to use
The user wants something removed from what Codewhale remembers about them.

## Setup
None. Forgetting is a review operation: the model proposes, the user
confirms in Context Lens. There is no silent-delete path, by design.

## Workflow
1. Identify the fact(s) in scope — quote what you believe is stored.
2. Propose removal through the memory capture path as a correction/removal
   proposal for Context Lens review.
3. Tell the user the proposal is pending their confirmation in Context Lens.
4. After confirmation, verify the fact is gone and say plainly what was forgotten.

## Non-goals
- Do not delete or overwrite stored memory directly. Ever.
- Do not "forget" by just not mentioning something — unconfirmed forgetting is lying.
- Do not forget session facts the current task still depends on without saying so.
