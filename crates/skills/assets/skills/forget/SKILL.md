---
name: forget
description: Help the user remove a stored fact using Codewhale's Context Lens when they ask to forget or delete a memory.
invocation: model+user
---

# Forget

Forgetting belongs to the user's Context Lens controls. The `remember` tool
supports append and revise candidates only; it cannot submit a deletion.
Do not invent a removal proposal or claim one is pending.

1. Identify the fact the user wants removed, quoting stored text only when it
   is actually available. Do not claim a fact exists merely because it was
   mentioned in the conversation.
2. Direct the user to select that fact in Context Lens and use its forget
   action. If the host has no Context Lens, say that this action is unavailable
   there; do not substitute direct edits to memory files or a fake tool call.
3. State that nothing has been deleted until the control reports completion.
   Verify removal when the host exposes that evidence; otherwise attribute the
   confirmation to the user.

Do not overwrite memory with an empty or contradictory note to simulate deletion.
Removing stored memory does not erase the existing conversation or its history.
