---
name: image-search
description: Find images on the web and save them locally. Use when: find an image, image search, stock photo, picture of, download an image, or need a visual.
invocation: model+user
---

# Image Search

## When to use
Finding a picture of something, gathering reference images, or fetching a
usable visual for a document, page, or deck.

## Setup
None — web research plus local download. Fail loud when the network policy
denies the search or fetch.

## Workflow
1. Search with image-oriented queries (subject + `photo`, `image`, or `stock`).
2. Shortlist with source page URLs and stated licenses; prefer openly
   licensed images when the use is publication.
3. Download only the chosen images (`curl -L -o file`) and verify each file
   opens before reporting its path.
4. Report path, dimensions, source, and license for every saved image.

## Non-goals
- Do not hotlink in place of downloading when a local copy was asked for.
- Do not strip watermarks or launder rights-managed images.
- Do not download bulk image sets unprompted.
