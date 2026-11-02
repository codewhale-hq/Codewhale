---
name: photos
description: Search and export the macOS Photos library. Use when: photos, photo library, pictures, find a photo, or media library.
invocation: model+user
---

# Photos

## When to use
Finding pictures in the user's Mac photo library by date, place, person,
album, or keyword — and exporting copies elsewhere.

## Setup
Requires `osxphotos` (`pip install osxphotos`) and a local Photos library.
Fail loud when either is missing. Full Disk Access may be needed for the
terminal on first use — say so when queries return nothing.

## Workflow
1. Query, don't browse: `osxphotos query --json --added-before/after ...`
   with the narrowest date/place/person filter the question allows.
2. Report counts first; export only what the user asked for:
   `osxphotos export /path --query ...`.
3. Describe image contents from the files, never from filenames alone.

## Non-goals
- Do not modify or delete library photos.
- Do not upload photos anywhere.
- Do not run face recognition beyond what Photos already computed.
