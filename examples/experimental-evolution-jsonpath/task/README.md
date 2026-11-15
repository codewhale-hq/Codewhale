# JSONPath query CLI

Implement `src/jsonpath_cli.py` with only the Python standard library. Read
one JSON object from stdin with exactly `document` (any JSON value) and `path`
(a string), and print one JSON response on one line. Valid paths produce
`{"ok":true,"matches":[...]}`. Invalid request shapes produce
`{"ok":false,"error":"invalid_request"}`. Invalid path syntax produces
`{"ok":false,"error":"invalid_path"}`. No other stdout.

The supported path grammar starts with `$`, followed by zero or more segments:

- `.name` selects an object member, where `name` matches
  `[A-Za-z_][A-Za-z0-9_]*`.
- `["member"]` selects an object member with any JSON string key; parse the
  quoted string with JSON escape rules.
- `[index]` selects an array element. Indices are signed decimal integers;
  negative indices count from the end. Out-of-range selections produce no
  match.
- `.*` or `[*]` selects every immediate child. Object values follow JSON
  insertion order; array elements follow index order.
- `..name` selects every descendant object member named `name` in depth-first,
  preorder traversal. `..*` selects every descendant child in the same order.
- `[start:end]` and `[start:end:step]` select array slices using Python slice
  semantics. Start and end are optional signed integers. Step is optional,
  signed, and may not be zero. Every selected element becomes a match.
- `[?(@.name==literal)]` filters elements of an array. Only object elements
  with member `name` are considered. `literal` is one complete JSON literal
  (string, number, boolean, null, array or object). Compare JSON values
  recursively: booleans differ from numbers, JSON numbers compare by numeric
  value, and object key order does not matter.

Apply each segment independently to each current match, concatenating results
in current-match order. A missing member or selection against the wrong type
produces no match. Keep duplicate matches if separate traversals reach the
same value. `$` alone returns the root as one match. A malformed segment,
unsupported operator, invalid JSON string escape or literal, zero slice step,
or trailing characters is `invalid_path` even if no data node could match it.
The entire path must parse before evaluation.

Modify only `src/`. Do not inspect the host evaluator, held-out cases, or
other candidate worktrees.
