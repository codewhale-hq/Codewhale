# Native font containers

These TrueType variable fonts are decoded copies of the canonical web fonts in
the parent directory. They contain the same font version, outlines, glyphs,
character maps, names, metrics and variation tables.

| Native asset | Canonical source | Family / PostScript name |
| --- | --- | --- |
| [Geist-Variable.ttf](Geist-Variable.ttf) | [Geist-Variable.woff2](../Geist-Variable.woff2) | Geist / Geist-Regular |
| [GeistMono-Variable.ttf](GeistMono-Variable.ttf) | [GeistMono-Variable.woff2](../GeistMono-Variable.woff2) | Geist Mono / GeistMono-Regular |

Both are Version 1.900 with the `wght` axis from 100 through 900, default 400.
They remain licensed under the parent [SIL Open Font License](../OFL.txt).

To regenerate, use FontTools with Brotli support, load each parent WOFF2 with
`TTFont(path, recalcTimestamp=False)`, set `font.flavor = None`, and save to
the corresponding TTF above. Keep the original source and font metadata.
The conversion changes the container checksum, not the design.

Native loading and variable-weight rendering must be verified in each consuming
client. These files alone do not establish iOS or Android support.
