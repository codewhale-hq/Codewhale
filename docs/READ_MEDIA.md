# `read_media` Operator Guide

`read_media` is a safe, first-class image reading and preprocessing tool for Codewhale v0.9.10. It allows vision-capable coding models to inspect visual assets (diagrams, UI mockups, screenshots, rendered graphs) with strict security, memory bounds, and privacy guards.

---

## 1. Overview and Scope

- **Supported Formats:** PNG, JPEG, GIF, WebP.
- **Out of Scope:** Video files, audio streams, and background/automatic screenshot watching are deliberately excluded.
- **Provider-Neutral Wiring:** Decoded and normalized images are converted into native image parts across all supported providers (OpenAI Chat Completions, Anthropic Messages, and OpenAI Responses API).

### Show the agent a screenshot

- Paste a clipboard image into the composer with `Ctrl+V`, or run
  `/attach <path>` for an existing PNG, JPEG, GIF, or WebP.
- Drag an image file onto the terminal, or paste its absolute path (plain,
  quoted, shell-escaped, or as a `file://` URL), and it attaches the same way.
  A paste becomes an attachment only when every path in it names an existing
  image; otherwise it stays text. A text-only model gets a notice instead of
  the image.
- Every attached image is drawn above the composer as a truecolor cell
  thumbnail, laid out left to right and sharing the row, with the file name
  (one image) or the count (several) under it. The
  picture is ordinary cell output (`▀` half blocks, top pixel as foreground,
  bottom as background), not a graphics protocol, so it renders on Windows
  Terminal, iTerm2, Terminal.app, GNOME Terminal, Konsole and WezTerm alike.
  A terminal that reports only 256 or 16 colors - Terminal.app among them -
  receives a downsampled palette.
  It is skipped in ASCII-safe mode, and it costs no rows while the decode is
  still running.
- Temporary macOS `NSIRD_screencaptureui` paths are copied into Codewhale's
  stable attachment store when ingested.
- Ask the agent to inspect the screenshot. The image is sent only as part of
  that explicit turn action; merely having a screenshot path or artifact does
  not trigger background analysis.

### Pasted image storage

A pasted clipboard image is saved as `clipboard-<number>.png` under
`~/.codewhale/clipboard-images/` (or `<workspace>/clipboard-images/` when no
home directory is available), and the attachment names that absolute path.
The image bytes enter the request when the message is built, so a later
missing file never fails a resume; editing and resending an old message
reattaches from the path and reports a missing image instead of sending it.

Nothing deletes these files on its own. To reclaim space:

```sh
codewhale sessions prune-images                 # report only
codewhale sessions prune-images --apply         # delete what the report lists
codewhale sessions prune-images --older-than-days 30
```

The command lists only images that no file under the Codewhale state
directory, the sessions directory or the Runtime task store still names
(saved sessions, checkpoints, archives, Runtime threads, the composer stash
and history) and that are older than the grace window (7 days by default),
so a draft or a running turn keeps its picture. If any file there cannot be
read, it reports what it found and deletes nothing. A file another process
still holds open (common on Windows) is reported and skipped. Close open
Codewhale sessions before `--apply`.

### How `Ctrl+V` reaches Codewhale

Most terminals own the paste chord, and Windows Terminal turns `Ctrl+V` into a
paste of the clipboard's *text*. When the clipboard holds only a picture there
is no text — but Windows Terminal still sends an **empty** bracketed paste.
That is deliberate (`// LOAD BEARING: Send an empty bracketed paste even if the
clipboard was empty` in its paste handler): it is how an application learns
that the person pasted even when the clipboard carried nothing the terminal
could read. Codewhale treats an empty paste as "ask the clipboard yourself"
and attaches the picture.

If a paste does nothing at all:

- The terminal may not honor bracketed paste. Codewhale enables it at startup,
  but a terminal that ignores the request never sends the event.
- In an SSH session with no forwarded graphical display the remote host cannot
  read your clipboard; paste stays terminal-owned and delivers text only.

`read_media` is the corresponding agent-side path for inspecting another
image later in the task without requiring the operator to attach it again.
The everyday `read` tool also returns a PNG, JPEG, GIF, or WebP as image
content (downscaled when over the inline limit). Both may open an image the
user attached from outside the workspace — that exact file, not its
directory.

---

## 2. Activation and Catalog Policy

- **Default-Off / Deferred Loading:** To preserve model context budgets, `read_media` is registered as a deferred tool (`defer_loading = true`) rather than occupying active slots in the default core catalog.
- **Explicit Model Invocation:** The tool is invoked explicitly by name when the model or user requests media inspection.
- **Eager Configuration:** Operators who want `read_media` always pre-loaded in the model tool catalog can configure:

```toml
[tools]
always_load = ["read_media"]
```

---

## 3. Tool Parameters and Schema

```json
{
  "path": "docs/architecture.png",
  "crop": {
    "x": 100,
    "y": 50,
    "width": 800,
    "height": 600
  },
  "detail": "auto"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **Yes** | Workspace-relative or trusted external path to the image file. |
| `crop` | object | No | Optional pixel bounding box `{ "x": u32, "y": u32, "width": u32, "height": u32 }` (0-indexed). |
| `detail` | string | No | Resolution target: `"auto"` (max 2048px, default), `"low"` (max 1024px), `"high"` / `"original"` (up to 4096px). |

---

## 4. Safety, Privacy, and Guardrails

### 4.1. Workspace Boundary and Credential Protection
- **Workspace Containment:** Paths must resolve within the workspace or user-approved trusted external paths (`/trust`). Symlink escapes outside trusted roots are rejected.
- **Credential Protection:** Codewhale configuration (`config.toml`, `.codewhale/`, `.deepseek/`, secrets directory) cannot be read via `read_media` and will fail with `PermissionDenied`.

### 4.2. Decompression-Bomb and Memory Limits
- **Source Byte Limit:** Maximum source file size before decoding is **20 MiB** (`MAX_SOURCE_IMAGE_BYTES`). Oversized files are rejected before allocation.
- **Dimension Guards:** Maximum permitted image width and height is **8192 px** (`MAX_IMAGE_DIMENSION`).
- **Pixel Budget:** Maximum total pixel count is **33,554,432 pixels** (~33.5 megapixels, `MAX_IMAGE_PIXELS`).
- **Memory Ceiling:** Safe memory allocation during decode is capped at **64 MiB** (`MAX_DECODE_ALLOC_BYTES`).
- **Wire Payload Limit:** Re-encoded image payload is capped at **5 MiB** (`MAX_WIRE_IMAGE_BYTES`), matching provider constraints.

### 4.3. Active Route Vision Checks
- Before reading an image, `read_media` inspects `context.route_capabilities.image_input`.
- If the active model route explicitly lacks vision support (`CapabilityState::Unsupported`), the tool returns an actionable error directing the operator to switch to a vision model:

```text
read_media: the active model route does not support image input. Switch to a route marked vision-capable with /model, or configure the route's image_input capability, then try again.
```

Only a known `Unsupported` capability blocks the explicit tool call. An
`Unknown` capability is admitted deliberately, matching normal attachment
routing: custom and self-hosted providers often do not publish modality
metadata, so their provider response remains authoritative. Operators who
need a fail-closed route can set its `image_input` capability explicitly.

---

## 5. Typed Receipts and Wire Integration

Each successful execution yields:
1. **Human-Readable Receipt (`content`):** Summarizes original format, source and final dimensions, crop details, and byte sizes.
2. **Typed JSON Metadata (`metadata`):** Contains structured dimension, crop, and byte information without exposing credentials or internal tokens.
3. **Rich Image Content Block (`content_blocks`):** Attaches a standardized `ToolResultContentBlock::Image` which provider adapters wire into outbound requests:
   - **OpenAI Chat Completions:** `image_url` block in following user message.
   - **Anthropic Messages:** `image` base64 source inside `tool_result` block.
   - **OpenAI Responses:** `input_image` block inside function call output.
