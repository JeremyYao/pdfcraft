# pdfcraft-audit

Layer L4 (may use `edit`): verification tooling, the faux-redaction audit.

```rust
let findings: Vec<Finding> = audit_redactions(&doc)?;  // page, kind, rect, covered_text
```

It reports two kinds of finding:

- **faux redaction**: an opaque filled rectangle painted *after* text it overlaps, dark enough
  against the text's colour (WCAG contrast below 3) to hide it, while the text is still
  extractable underneath;
- **unapplied mark**: a Redact annotation that was marked but never applied.

A properly applied redaction removes the text, so it never flags. Page backgrounds, table
shading and highlights don't either: they're painted before the text, or light against it.
A cover hides a line when it spans most of the line's height over at least a glyph's width, so
a box over one word of a sentence counts (and each of several boxes that together black out a
line), while an underline, a strikethrough or a table rule does not. The finding's
`covered_text` is the whole text of every line the cover touches, not only the hidden glyphs.

Limits, by design and documented rather than hidden:

- only filled rectangles count as covers, drawn with `re` or as a single `m`/`l` subpath whose
  corners form an axis-aligned rectangle (not other paths, shadings or patterns), and a
  translucent one (`/ca` below 1) is not flagged;
- covers inside form XObjects are found, but only text drawn directly in the page content is
  checked under them;
- annotation rectangles are compared in unrotated user space, so `/Rotate` pages may misalign.

It fails closed: an undecodable content stream, malformed `/Annots`, form XObjects nested deeper
than 32 or in a cycle, or a walk past the operator bound is an error, never "no findings".

Surfaces: the `redact_audit` automation tool and `pdfcraft-cli`.
