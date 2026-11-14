# Accept-Language negotiation task

Implement `src/accept_language.py` as a command-line program. Read one JSON
object from standard input with `header` (a string) and `available` (an array
of language tags). Write one JSON string containing the selected available tag,
or `null` when no tag is acceptable. The program must not print other output.

Parse comma-separated language ranges case-insensitively. A range is `*` or
ASCII alphabetic subtags separated by a single `-`; every subtag has 1–8
characters. Ignore malformed ranges. A range can match an available tag
exactly or match a longer tag by prefix at a `-` boundary. `*` matches every
available tag.

A range may have one optional `q` parameter; any other or repeated parameter
invalidates that range. The parameter name is case-insensitive, and spaces
around the range, `;`, and `=` are allowed. The value is exactly `0`, `1`,
`0.` followed by 1–3 decimal digits, or `1.` followed by 1–3 zeroes. The
default is 1. Ignore a range with an invalid value. Do not use permissive
floating-point parsing for this grammar.

A more specific range for the same available tag overrides a less specific
range, including when its quality is zero. Among equal-specificity matching
ranges, the earlier range wins, even if its quality is zero. Quality zero
excludes that tag.

Among acceptable tags, choose the highest quality. Break ties by range
specificity (more subtags first, wildcard last), then earlier range order,
then earlier order in `available`. An empty header accepts nothing.

Change only `src/`. Run the CLI locally before returning. The host will run
public and held-out process-level gates from a frozen baseline.
