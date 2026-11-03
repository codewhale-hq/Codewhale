---
name: google-calendar
description: List and manage Google Calendar events via the Calendar API. Use when: calendar, schedule, meeting, event, availability, or what's on today/this week.
invocation: model+user
---

# Google Calendar

## When to use
Reading schedules, checking availability, creating or updating events.

## Setup
Shares Google auth with the `gmail` skill. Fail loud when it is missing:

```
gcloud auth application-default login --scopes=openid,https://www.googleapis.com/auth/calendar,https://www.googleapis.com/auth/gmail.modify
TOKEN=$(gcloud auth application-default print-access-token)
```

The Calendar API must be enabled on the user's Google Cloud project.

## Workflow
1. Read: `GET calendar/v3/calendars/primary/events?timeMin=...&timeMax=...&singleEvents=true&orderBy=startTime`.
2. Always state the timezone you queried in; convert to the user's local zone.
3. Creates/updates/deletes need explicit user approval showing title, time,
   attendees, and location first.
4. For availability questions, report free/busy windows, not full event bodies.

## Non-goals
- Do not create, move, or delete events on your own initiative.
- Do not invite or remove attendees unless asked.
- Do not guess timezones — confirm when ambiguous.
