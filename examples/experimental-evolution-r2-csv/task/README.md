# CSV process CLI

Implement `src/csv_cli.py` as a deterministic one-request JSON process.

Read one JSON object from standard input. It must contain exactly `text` and
`delimiter`. `text` is a string. `delimiter` is either `,` or `;`.

For valid requests, parse the text with Python's standard `csv.reader` using
the requested delimiter, `strict=True`, and `io.StringIO(text, newline="")`.
Return `{"ok": true, "rows": [[...], ...]}` as one JSON object on standard
output. Preserve blank records, quoted delimiters, embedded CR/LF, escaped
quotes, Unicode, and exact field strings as `csv.reader` returns them.

For a malformed CSV record (`csv.Error`), return
`{"ok": false, "error": "invalid_csv"}`. For any other invalid input shape,
return `{"ok": false, "error": "invalid_request"}`. Do not print any other
text. Exit with status 0 for a handled request.
