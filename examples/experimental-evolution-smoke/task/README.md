# Accept-Language negotiation task

Implement `src/accept_language.py` as a command-line program. Read one JSON
object from standard input with `header` (a string) and `available` (an array
of language tags). Write one JSON string containing the selected available tag,
or `null` when no tag is acceptable. The program must not print other output.

Parse comma-separated language ranges case-insensitively. A range may carry a
`q` value between 0 and 1, inclusive; the default is 1. Ignore a range with
an invalid `q` value. A range can match an available tag exactly, match a
longer tag by prefix at a `-` boundary, or be `*`. A more specific range for
the same available tag overrides a less specific range, including when its
quality is zero. Quality zero excludes that tag.

Among acceptable tags, choose the highest quality. Break ties by range
specificity (more subtags first, wildcard last), then earlier range order,
then earlier order in `available`. An empty header accepts nothing.

Change only `src/`. Run the CLI locally before returning. The host will run
public and held-out process-level gates from a frozen baseline.
