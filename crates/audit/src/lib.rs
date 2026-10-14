//! Verification tooling: audit a document for faux redactions.
//!
//! [`audit_redactions`] finds *faux redactions* — opaque filled rectangles drawn over text
//! that is still extractable underneath — and redaction annotations that were marked but
//! never applied. A properly applied redaction removes the text, so it never flags.
//!
//! A rectangle counts as a cover only when it was painted strictly *after* the text it
//! overlaps and hides what is under it: opaque (`/ca` of 1) and in Normal or Compatible blend
//! mode. Its colour does not matter: a white-out box over black text, or a black box over grey
//! or coloured text, hides it as surely as black over black. Page backgrounds and table shading
//! painted before the text are therefore not flagged, nor are highlights drawn translucent or
//! with a Multiply blend mode, which leave the text visible. It hides a line when it spans most
//! of the line's height over at least a glyph's width: a box over one word of a sentence counts,
//! an underline, a strikethrough or a table rule does not. A finding reports the whole text of
//! every line the cover touches, not only the hidden glyphs.
//!
//! Covers are collected from the page content, the form XObjects it draws, and the normal
//! appearance stream of every annotation a viewer paints (not Hidden or NoView, not a popup),
//! placed by its `/Rect`, `/BBox` and `/Matrix` and painted after the page content: a box
//! drawn over text with a shape or highlight tool is audited like one in the content.
//!
//! Detection limits (documented, not hidden): only `re` rectangles closed by a fill operator
//! are considered — arbitrary filled paths, shadings and patterns are not; a rectangle drawn
//! with a translucent ExtGState (`/ca < 1`) or a blend mode other than Normal or Compatible is
//! not flagged; an opaque light rectangle painted over text for another reason (shading added
//! after the text) is flagged, since it does hide the text; annotation rectangles are compared
//! in unrotated user space, so pages with `/Rotate` may misalign; covers inside form XObjects
//! and annotation appearances are found, but only text drawn directly in the page content is
//! checked under them, so text inside a form XObject or an appearance is not audited; an
//! annotation whose `/AP /N` holds several states and no `/AS` has no appearance to show and
//! contributes nothing.
//!
//! The audit fails closed: any content that cannot be read (an undecodable content stream,
//! malformed annotations, form XObjects nested deeper than 32 or in a cycle) is an error, never
//! an empty finding list.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use pdfcraft_cos::{Dict, Document};
use pdfcraft_edit::TextLine;

mod cover;

#[cfg(test)]
mod tests;

/// What the audit found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindingKind {
    /// An opaque filled rectangle covers text that is still extractable.
    CoveredText,
    /// A `/Redact` annotation was placed but never applied: the text is untouched.
    UnappliedMark,
}

impl FindingKind {
    pub fn id(self) -> &'static str {
        match self {
            FindingKind::CoveredText => "covered-text",
            FindingKind::UnappliedMark => "unapplied-mark",
        }
    }
}

/// One audit finding.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditFinding {
    /// 0-based page index.
    pub page: usize,
    pub kind: FindingKind,
    /// The covering rectangle (or the mark's rectangle), in page user space.
    pub rect: [f64; 4],
    /// The extractable text the cover hides (possibly empty when nothing decodable remains).
    pub covered_text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("page {0}: {1}")]
    Text(usize, String),
    #[error("content walk exceeded the operator bound")]
    TooManyOps,
    #[error("form XObjects nest deeper than the audit follows")]
    TooDeep,
    #[error("page has a malformed /Annots entry")]
    BadAnnots,
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
    #[error(transparent)]
    Edit(#[from] pdfcraft_edit::EditError),
}

/// Fraction of a text line's height a cover must span to hide glyphs rather than underline or
/// strike them through.
const COVER_HEIGHT: f64 = 0.5;
/// A glyph's width as a fraction of the line's height: the least width a cover must overlap.
const GLYPH_WIDTH: f64 = 0.25;

/// Audit every page for faux redactions and unapplied redaction marks. Fails closed: any
/// content that cannot be decoded is returned as an error, never as an empty finding list.
pub fn audit_redactions(doc: &Document) -> Result<Vec<AuditFinding>, AuditError> {
    let mut out = Vec::new();
    for (n, page) in pdfcraft_model::pages(doc).iter().enumerate() {
        let lines = pdfcraft_edit::text_lines(doc, n).map_err(|e| AuditError::Text(n, e.to_string()))?;
        let annotations = annotations(doc, &page.dict)?;
        // 1. Opaque filled rectangles over extractable text, in the page content and in the
        //    appearances of its annotations.
        for r in cover::filled_rects(doc, &page.dict, &annotations)? {
            let mut covered = String::new();
            for line in &lines {
                if is_cover(&r, line) {
                    if !covered.is_empty() {
                        covered.push(' ');
                    }
                    covered.push_str(line.text.trim());
                }
            }
            if !covered.trim().is_empty() {
                out.push(AuditFinding { page: n, kind: FindingKind::CoveredText, rect: r.rect, covered_text: covered });
            }
        }
        // 2. Redact annotations that were never applied.
        for annot in redact_marks(doc, &annotations) {
            let mut covered = String::new();
            for line in &lines {
                if covers(&annot, &line.rect) {
                    if !covered.is_empty() {
                        covered.push(' ');
                    }
                    covered.push_str(line.text.trim());
                }
            }
            if !covered.trim().is_empty() {
                out.push(AuditFinding { page: n, kind: FindingKind::UnappliedMark, rect: annot, covered_text: covered });
            }
        }
    }
    Ok(out)
}

/// A filled rectangle is a faux-redaction cover for a text line iff it was painted strictly
/// after the line and hides glyphs of it. Its colour does not matter: an opaque fill hides the
/// text whatever the two colours are. Fills that cannot hide it (translucent, or in a blend
/// mode such as Multiply, as highlights are) are left out by the cover walker.
fn is_cover(rect: &cover::FilledRect, line: &TextLine) -> bool {
    rect.pos > line.paint_pos() && covers(&rect.rect, &line.rect)
}

/// Does `cover` hide glyphs of `line`? It must span most of the line's height (an underline, a
/// strikethrough or a table rule does not) over at least a glyph's width (a quarter of the
/// line's height, or all of a shorter line), so a box over one word of a long line counts, and
/// so does each of several boxes that together black out the line. The line's whole text is
/// reported for it: lines are not split into glyphs here. Any NaN compares false: no cover.
fn covers(cover: &[f64; 4], line: &[f64; 4]) -> bool {
    let w = cover[2].min(line[2]) - cover[0].max(line[0]);
    let h = cover[3].min(line[3]) - cover[1].max(line[1]);
    let line_w = line[2] - line[0];
    let line_h = line[3] - line[1];
    if !(w > 0.0 && h > 0.0 && line_w > 0.0 && line_h > 0.0) {
        return false;
    }
    h >= line_h * COVER_HEIGHT && w >= (line_h * GLYPH_WIDTH).min(line_w)
}

/// The annotation dictionaries of a page, in `/Annots` order. A missing `/Annots` means no
/// annotations; a malformed one fails the audit rather than silently passing.
fn annotations(doc: &Document, page: &Dict) -> Result<Vec<Dict>, AuditError> {
    let Some(a) = page.get(b"Annots") else { return Ok(Vec::new()) };
    let resolved = doc.resolve(a);
    let items: Vec<pdfcraft_cos::Object> = match &*resolved {
        pdfcraft_cos::Object::Array(items) => items.clone(),
        _ => return Err(AuditError::BadAnnots),
    };
    Ok(items
        .iter()
        .filter_map(|a| match &*doc.resolve(a) {
            pdfcraft_cos::Object::Dict(d) => Some(d.clone()),
            _ => None,
        })
        .collect())
}

/// Rectangles of the `/Redact` annotations among `annotations` (unrotated user space).
fn redact_marks(doc: &Document, annotations: &[Dict]) -> Vec<[f64; 4]> {
    annotations.iter().filter(|a| a.name(b"Subtype") == Some(b"Redact")).filter_map(|a| cover::rect(doc, a.get(b"Rect"))).collect()
}
