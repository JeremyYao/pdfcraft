//! Clearing the pixels of an image under redaction regions. The image is decoded, every pixel
//! whose centre maps into a region is set to zero (no ink for image masks), and the result is
//! written as a new Flate image; the original object is untouched (other pages may use it).
//! The image's soft mask or stencil mask (`/SMask`, a `/Mask` stream) carries samples of the same
//! pixels, so it is cleared over the same regions into a copy of its own.
//! Images whose codec PdfCraft can't decode (DCT, JPX, JBIG2, CCITT), or whose sample layout is
//! not certain to be the one the page is drawn with (see [`components`] and [`SHORT_KEYS`]),
//! return `None`, and the caller removes the whole image instead; that is fail-closed. So does a
//! mask that can't be cleared.

use pdfcraft_content::{Matrix, overlaps};
use pdfcraft_cos::{Dict, Document, Filter, Object, Stream};

/// Most pixels an image may have to be cleared in place (larger ones are removed).
const MAX_PIXELS: u64 = 64 << 20;

/// The inline-image abbreviations (ISO 32000-2 Table 92) that stand for the keys read here. They
/// belong to inline images only, but the renderer reads them before the full keys on any image,
/// so a dictionary carrying one may be drawn in another layout, or from other data, than the full
/// keys describe.
const SHORT_KEYS: [&[u8]; 8] = [b"CS", b"W", b"H", b"BPC", b"IM", b"D", b"F", b"DP"];

/// Components per pixel, only when the renderer cannot arrive at another count: it resolves a
/// bare name for the Device spaces alone, takes an ICC profile's /Alternate when the profile does
/// not load (and /N only for 1, 3 or 4 without one), and draws a colour space it cannot resolve
/// as DeviceGray. `None` when the count is not certain (the caller removes the image).
fn components(doc: &Document, s: &Stream) -> Option<usize> {
    let device = |n: &[u8]| match n {
        b"DeviceGray" => Some(1),
        b"DeviceRGB" => Some(3),
        b"DeviceCMYK" => Some(4),
        _ => None,
    };
    let cs = doc.resolve(s.dict.get(b"ColorSpace")?);
    let Object::Array(a) = &*cs else { return device(cs.as_name()?) };
    let first = doc.resolve(a.first()?);
    let name = first.as_name()?;
    if let Some(n) = device(name) {
        return Some(n);
    }
    match name {
        // One component whether or not the renderer resolves them.
        b"CalGray" | b"Indexed" | b"I" | b"Separation" => Some(1),
        // Without their dictionary the renderer resolves nothing and draws DeviceGray.
        b"CalRGB" | b"Lab" => matches!(&*doc.resolve(a.get(1)?), Object::Dict(_)).then_some(3),
        b"ICCBased" => {
            let icc = doc.resolve(a.get(1)?);
            let Object::Stream(icc) = &*icc else { return None };
            let n = match icc.dict.int(b"N")? {
                1 => 1,
                3 => 3,
                4 => 4,
                _ => return None,
            };
            // The alternate is drawn when the profile does not load: only one of the same count.
            if let Some(alt) = icc.dict.get(b"Alternate") {
                let alt = doc.resolve(alt);
                let alt_name = match &*alt {
                    Object::Name(n) => n.as_slice(),
                    Object::Array(a) => a.first()?.as_name()?,
                    _ => return None,
                };
                if device(alt_name)? != n {
                    return None;
                }
            }
            // A profile the renderer cannot decode is drawn as DeviceGray.
            if n != 1 && icc.decoded_strict_in(doc).is_err() {
                return None;
            }
            Some(n)
        }
        _ => None,
    }
}

/// `Ok(Some(copy))` with the covered pixels cleared (its masks cleared into new objects),
/// `Ok(None)` when no pixel centre is covered, `Err(())` when the image can't be cleared (the
/// caller removes it).
#[allow(clippy::result_unit_err)]
pub(crate) fn clear(doc: &mut Document, s: &Stream, ctm: &Matrix, rects: &[[f64; 4]]) -> Result<Option<Stream>, ()> {
    let Some(cleared) = decode_and_clear(doc, s, ctm, rects) else { return Err(()) };
    let Some(Cleared { mut dict, data, masks }) = cleared else { return Ok(None) };
    for (key, mask) in masks {
        let r = doc.add(Object::Stream(mask));
        dict.set(key, Object::Ref(r));
    }
    Ok(Some(Stream::flate(dict, &data)))
}

/// A cleared image: its dictionary (without the entries the new data makes stale), its samples,
/// and the cleared copies of its masks to store under the given keys.
struct Cleared {
    dict: Dict,
    data: Vec<u8>,
    masks: Vec<(Vec<u8>, Stream)>,
}

fn decode_and_clear(doc: &Document, s: &Stream, ctm: &Matrix, rects: &[[f64; 4]]) -> Option<Option<Cleared>> {
    let Some((mut dict, data)) = clear_samples(doc, s, ctm, rects)? else { return Some(None) };
    // Alternate images hold the same pixels again; the copy does without them.
    dict.remove(b"Alternates");
    dict.remove(b"OPI");
    // The masks are drawn through the same unit square, so the same regions cover them, whatever
    // their resolution. A mask that can't be cleared fails the whole image (removed by the caller).
    let mut masks = Vec::new();
    for key in [&b"SMask"[..], b"Mask"] {
        let Some(m) = s.dict.get(key) else { continue };
        match &*doc.resolve(m) {
            // `None` from the mask: no mask pixel centre under the regions (a coarser mask), so
            // it is kept as it is.
            Object::Stream(mask) => {
                if let Some((mut md, data)) = clear_samples(doc, mask, ctm, rects)? {
                    // A mask has no masks of its own.
                    md.remove(b"SMask");
                    md.remove(b"Mask");
                    masks.push((key.to_vec(), Stream::flate(md, &data)));
                }
            }
            // A colour-key mask (an array) holds no samples; anything else is not a mask.
            Object::Array(_) => {}
            _ => {
                dict.remove(key);
            }
        }
    }
    Some(Some(Cleared { dict, data, masks }))
}

/// Decode one image's samples and clear the pixels under the regions: `Some(Some((dict, data)))`
/// with the copy's dictionary (`/Length` and the decode parameters removed) and the new samples,
/// `Some(None)` when no pixel centre is covered, `None` when the image can't be cleared.
fn clear_samples(doc: &Document, s: &Stream, ctm: &Matrix, rects: &[[f64; 4]]) -> Option<Option<(Dict, Vec<u8>)>> {
    // The chain as the document declares it (indirect entries resolved); only codecs that can be
    // decoded and re-encoded here.
    let chain = s.filters_in(doc).ok()?;
    let supported = |f: &Filter| matches!(f, Filter::Flate | Filter::Lzw | Filter::AsciiHex | Filter::Ascii85 | Filter::RunLength);
    if !chain.iter().all(|(f, _)| supported(f)) || SHORT_KEYS.iter().any(|k| s.dict.contains(k)) {
        return None;
    }
    let (w, h) = (s.dict.int(b"Width")?, s.dict.int(b"Height")?);
    if w <= 0 || h <= 0 || (w as u64).checked_mul(h as u64).is_none_or(|n| n > MAX_PIXELS) {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let mask = matches!(s.dict.get(b"ImageMask"), Some(Object::Bool(true)));
    let (ncomp, bpc) = if mask { (1, 1) } else { (components(doc, s)?, s.dict.int(b"BitsPerComponent")? as usize) };
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return None;
    }
    // Strict: samples a viewer would decode differently (a corrupt or partial stream) are not
    // cleared in place at offsets of their own.
    let mut data = s.decoded_strict_in(doc).ok()?;
    let row = w.checked_mul(ncomp)?.checked_mul(bpc)?.div_ceil(8);
    let need = row.checked_mul(h)?;
    if data.len() < need {
        return None;
    }
    data.truncate(need);
    // Image masks: a sample of 0 paints unless /Decode is [1 0]; cleared samples must not paint.
    let clear_bit = if mask {
        let inverted = s.dict.get(b"Decode").and_then(Object::as_array).and_then(|d| d.first()).and_then(Object::as_f64) == Some(1.0);
        !inverted
    } else {
        false
    };
    let bits_per_pixel = ncomp * bpc;
    let mut cleared = 0usize;
    for j in 0..h {
        // Image space: (0,0) is the bottom-left of the unit square; row 0 is the top.
        let v = 1.0 - (j as f64 + 0.5) / h as f64;
        let (ax, ay) = ctm.apply(0.0, v);
        let (bx, by) = ctm.apply(1.0, v);
        let row_box = [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)];
        if !rects.iter().any(|r| overlaps(*r, [row_box[0], row_box[1] - 0.01, row_box[2], row_box[3] + 0.01], 0.0)) {
            continue;
        }
        for i in 0..w {
            let u = (i as f64 + 0.5) / w as f64;
            let (x, y) = ctm.apply(u, v);
            if !rects.iter().any(|r| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]) {
                continue;
            }
            cleared += 1;
            let start = j * row * 8 + i * bits_per_pixel;
            for bit in start..start + bits_per_pixel {
                let (byte, shift) = (bit / 8, 7 - bit % 8);
                if clear_bit {
                    data[byte] |= 1 << shift;
                } else {
                    data[byte] &= !(1 << shift);
                }
            }
        }
    }
    if cleared == 0 {
        return Some(None);
    }
    let mut dict = s.dict.clone();
    dict.remove(b"Length");
    dict.remove(b"DecodeParms");
    Some(Some((dict, data)))
}
