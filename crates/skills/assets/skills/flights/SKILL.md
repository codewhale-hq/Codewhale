---
name: flights
description: Track flights and look up status and schedules. Use when: flight status, track a flight, flightaware, arrivals, departures, delays, or a flight number.
invocation: model+user
---

# Flights

## When to use
Live status, schedules, gates, and delays for a flight the user names.

## Setup
Two paths:

1. **Web (no setup).** Flight number + date via web search; good enough for
   status, terminals, and delays.
2. **FlightAware AeroAPI (structured data).** Needs the user's API key, or
   fail loud:
   `curl -H "x-apikey: $FLIGHTAWARE_API_KEY" https://aeroapi.flightaware.com/aeroapi/flights/{ident}`

## Workflow
1. Confirm the flight identifier and date — never guess between same-number
   daily flights.
2. Report status, scheduled vs actual times, origin/destination, and terminal/gate.
3. State the timezone of every time quoted.

## Non-goals
- Do not book, change, or cancel travel.
- Do not look up other people's itineraries from a name alone.
- Do not quote times without their timezone.
