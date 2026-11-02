---
name: gmail
description: Search, read, draft, and send Gmail via the Gmail API. Use when: email, inbox, unread mail, sending mail, newsletters, or anything gmail.
invocation: model+user
---

# Gmail

## When to use
Reading, searching, drafting, or sending email in the user's Gmail account.

## Setup
Requires Google auth with a Gmail scope. Fail loud when it is missing:

```
gcloud auth application-default login --scopes=openid,https://www.googleapis.com/auth/gmail.modify,https://www.googleapis.com/auth/calendar
TOKEN=$(gcloud auth application-default print-access-token)
```

The Gmail API must be enabled on the user's Google Cloud project. Never ask
for or store the user's Google password — OAuth tokens only.

## Workflow
1. Search first, read second: `GET gmail/v1/users/me/messages?q=...`, then
   `GET gmail/v1/users/me/messages/{id}?format=full`.
2. Summarize threads; quote sender, date, and subject for anything actionable.
3. Drafts are safe to create; **sending needs explicit user approval every
   time**, showing recipient, subject, and full body first.
4. Attachments: download via `messages.attachments.get`, never execute them.

## Non-goals
- Do not send, delete, or archive mail on your own initiative.
- Do not subscribe, unsubscribe, or change filters/labels unless asked.
- Do not paste credentials or tokens into chat or files outside the auth flow.
