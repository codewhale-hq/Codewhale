# JSON Patch transaction CLI

Implement `src/patch_cli.py` using the Python standard library. Read one JSON
request from standard input and print one JSON response on one line. The
request has exactly this shape: `{"document": <any JSON value>,
"operations": [<operation>, ...]}`. Process operations in order. A successful
response is `{"ok": true, "document": <result>}`. On the first error return
`{"ok": false, "error": {"index": <zero-based operation index>, "code":
"<code>"}}`; include no partial document. An invalid top-level request has
index `-1` and code `invalid_request`. Write no other stdout.

Operation objects use these forms:

- `add`: `{"op":"add","path":pointer,"value":value}`. Add or overwrite an
  object key. Insert before an array index, or use `-` to append.
- `remove`: `{"op":"remove","path":pointer}`. The target must exist.
- `replace`: `{"op":"replace","path":pointer,"value":value}`. The target
  must exist.
- `copy`: `{"op":"copy","from":pointer,"path":pointer}`. Deep copy the
  source value and add it at the destination.
- `move`: `{"op":"move","from":pointer,"path":pointer}`. Remove the source,
  then add its value at the destination in the modified document. Moving a
  location to itself succeeds unchanged. Moving a node into its own descendant
  fails with `move_into_child`.
- `test`: `{"op":"test","path":pointer,"value":value}`. Compare JSON
  values recursively. Boolean is distinct from number. JSON numbers compare by
  numeric value; object key order is irrelevant. A mismatch is `failed_test`.

Pointers are RFC 6901 style. The empty pointer denotes the entire document.
Otherwise it starts with `/`; each token decodes `~1` to `/` and `~0` to `~`.
Any other tilde escape is `invalid_pointer`. Empty object keys are allowed.
For arrays, indices are decimal integers with no sign and no leading zero
except `0`. `-` is valid only at the final target of `add`, `copy` or `move`.
Malformed or out-of-range array indices are `invalid_index`. When traversing
objects, a missing key or attempting to descend through a scalar is
`missing_path`. The parent of a destination must already exist. A root `add`
or `replace` replaces the whole document; root `remove` yields JSON `null`.

Missing required operation fields, wrong field types, and unknown operation
names are `invalid_operation`. These checks take precedence before pointer
resolution. All changes are atomic at the request level: after any failure,
the response contains only the error. Never read a hidden evaluator or other
candidate worktree. Modify only `src/`.
