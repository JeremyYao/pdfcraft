# pdfcraft-audit

Layer L4 (may use `edit`): verification tooling, the faux-redaction audit.

```rust
let findings: Vec<Finding> = audit_redactions(&doc)?;  // page, kind, rect, covered_text
```

It reports two kinds of finding:

- **faux redaction**: an opaque filled rectangle painted *after* text it overlaps, while the
  text is still extractable underneath. Its colour doesn't matter: a white-out box over black
  text, or a black box over grey or coloured text, hides it as surely as black over black;
- **unapplied mark**: a Redact annotation that was marked but never applied.

A properly applied redaction removes the text, so it never flags. Page backgrounds and table
shading painted before the text don't either, nor do highlights drawn translucent (`/ca` below
1) or with a blend mode such as Multiply, which leave the text visible.
A cover hides a line when it spans most of the line's height over at least a glyph's width, so
a box over one word of a sentence counts (and each of several boxes that together black out a
line), while an underline, a strikethrough or a table rule does not. The finding's
`covered_text` is the whole text of every line the cover touches, not only the hidden glyphs.

Covers come from the page content, the form XObjects it draws, and the normal appearance of
every annotation a viewer paints (not Hidden or NoView, not a popup), placed by its `/Rect`,
`/BBox` and `/Matrix` (ISO 32000-2 §12.5.5) and painted after the page content: a box drawn
over text with a shape or highlight tool is audited like one in the content.

Limits, by design and documented rather than hidden:

- only filled rectangles count as covers, drawn with `re` or as a single `m`/`l` subpath whose
  corners form an axis-aligned rectangle (not other paths, shadings or patterns), and one that
  is translucent (`/ca` below 1) or in a blend mode other than Normal or Compatible is not
  flagged; an opaque light rectangle painted over text for another reason (shading added after
  the text) is flagged, since it does hide the text;
- covers inside form XObjects and annotation appearances are found, but only text drawn
  directly in the page content is checked under them;
- an annotation whose `/AP /N` holds several states and no `/AS` has no appearance to show and
  contributes nothing;
- annotation rectangles are compared in unrotated user space, so `/Rotate` pages may misalign.

It fails closed: an undecodable content or appearance stream, malformed `/Annots`, form
XObjects nested deeper than 32 or in a cycle, or a walk past the operator bound is an error,
never "no findings".

Surfaces: the `redact_audit` automation tool and `pdfcraft-cli`.
