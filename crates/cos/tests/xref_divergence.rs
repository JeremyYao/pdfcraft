//! Cross-reference entries another reader resolves differently from this one are recorded as
//! oddities, so that code comparing revisions (signature validation) never calls such an object
//! unchanged.

use std::sync::Arc;

use pdfcraft_cos::{Document, XrefEntry};

/// A classic-xref file from object bodies (object i+1 = bodies[i]).
/// Mirrors `document::tests::build`, which an integration test cannot reach.
fn build(bodies: &[&str], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    out
}

/// A base file, the offset of its cross-reference section and where object 3 sits.
fn base() -> (Vec<u8>, usize, XrefEntry, u64) {
    let bytes = build(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [] /Count 0 >>", "(three)"], "/Root 1 0 R");
    let prev = String::from_utf8_lossy(&bytes).rfind("xref\n0 4").unwrap();
    let third = Document::open(Arc::new(bytes.clone())).unwrap().xref_entry(3).unwrap();
    let XrefEntry::InFile { offset, .. } = third else { panic!("object 3 is in the file") };
    (bytes, prev, third, offset)
}

/// A classic entry whose offset the 10-digit field holds but 32 bits do not is set aside, and
/// an entry that is not the fixed 20 bytes is kept but recorded: a reader that counts bytes, or
/// parses the field into 32 bits, reads the table differently.
#[test]
fn classic_entries_another_reader_refuses_are_recorded() {
    let (base, prev, third, third_at) = base();
    let mut bytes = base;
    let section = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n3 1\n{third_at:010} 00000 n\n5 1\n9999999999 00000 n \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {prev} >>\nstartxref\n{section}\n%%EOF\n")
            .as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert_eq!(doc.xref_oddities(), [(section as u64, 3), (section as u64, 5)]);
    assert_eq!(doc.xref_entry(3), Some(third), "the 19-byte entry still names the object");
    assert_eq!(doc.xref_entry(5), None, "the wide entry is set aside");
}

/// Cross-reference stream rows a byte-counting reader gets wrong: a type field two bytes wide,
/// and a row /Index declares that the data does not hold.
#[test]
fn stream_rows_another_reader_gets_wrong_are_recorded() {
    let (base, prev, third, third_at) = base();
    let third_at = [(third_at >> 8) as u8, third_at as u8];
    let cases: [(&str, &str, Vec<u8>, u32); 2] =
        [("[2 2 1]", "[3 1]", vec![0, 1, third_at[0], third_at[1], 0], 3), ("[1 2 1]", "[3 2]", vec![1, third_at[0], third_at[1], 0], 4)];
    for (w, index, data, expect) in cases {
        let mut bytes = base.clone();
        let section = bytes.len();
        bytes.extend_from_slice(
            format!("9 0 obj\n<< /Type /XRef /Size 10 /W {w} /Index {index} /Root 1 0 R /Prev {prev} /Length {} >>\nstream\n", data.len()).as_bytes(),
        );
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{section}\n%%EOF\n").as_bytes());
        let doc = Document::open(Arc::new(bytes)).unwrap();
        assert_eq!(doc.xref_oddities(), [(section as u64, expect)], "/W {w} /Index {index}");
        assert_eq!(doc.xref_entry(3), Some(third), "/W {w} /Index {index}");
    }
}

/// A hybrid file's cross-reference stream may carry a /Prev of its own. This reader follows the
/// table's (§7.5.8.4); what the stream's chain resolves differently is recorded.
#[test]
fn a_hybrid_streams_own_prev_chain_is_recorded_where_it_disagrees() {
    let (base, prev, third, _) = base();
    let mut bytes = base;
    // A second body for object 3, listed only by an older section that the stream's /Prev names.
    let body = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n(other)\nendobj\n");
    let older = bytes.len();
    bytes.extend_from_slice(format!("xref\n3 1\n{body:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\n").as_bytes());
    let stream = bytes.len();
    bytes.extend_from_slice(
        format!("8 0 obj\n<< /Type /XRef /Size 9 /W [1 2 1] /Index [0 0] /Prev {older} /Length 0 >>\nstream\n\nendstream\nendobj\n").as_bytes(),
    );
    let table = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 0\ntrailer\n<< /Size 9 /Root 1 0 R /XRefStm {stream} /Prev {prev} >>\nstartxref\n{table}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert_eq!(doc.xref_entry(3), Some(third), "the table's /Prev chain is the one followed");
    assert_eq!(doc.xref_oddities(), [(table as u64, 3)]);

    // The same stream /Prev naming the section the table's /Prev names too: nothing disagrees.
    let (bytes, prev, _, _) = self::base();
    let mut bytes = bytes;
    let stream = bytes.len();
    bytes.extend_from_slice(
        format!("8 0 obj\n<< /Type /XRef /Size 9 /W [1 2 1] /Index [0 0] /Prev {prev} /Length 0 >>\nstream\n\nendstream\nendobj\n").as_bytes(),
    );
    let table = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 0\ntrailer\n<< /Size 9 /Root 1 0 R /XRefStm {stream} /Prev {prev} >>\nstartxref\n{table}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert!(doc.xref_oddities().is_empty(), "{:?}", doc.xref_oddities());
}

/// An object body that no cross-reference entry locates: a reader that rebuilds its table by
/// scanning the file takes it for the number, this reader does not.
#[test]
fn object_bodies_no_entry_locates_are_listed() {
    let (base, prev, _, _) = base();
    let doc = Document::open(Arc::new(base.clone())).unwrap();
    assert!(doc.objects_unlisted_from(0).is_empty());
    // A second body for object 3 after the file, listed by nothing.
    let mut bytes = base;
    let body = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n(other)\nendobj\n");
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    assert_eq!(doc.objects_unlisted_from(body), [3]);
    assert!(doc.objects_unlisted_from(bytes.len()).is_empty());
    // The same body listed by an appended section: located, so not unlisted.
    let section = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n3 1\n{body:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {prev} >>\nstartxref\n{section}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    assert!(doc.objects_unlisted_from(body).is_empty(), "{:?}", doc.objects_unlisted_from(body));
    // A later revision defining object 3 once more leaves the earlier body listed by the section
    // that placed it there: a redefinition is not an unlisted body, however many follow.
    let body2 = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n(newer)\nendobj\n");
    let section2 = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n3 1\n{body2:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {section} >>\nstartxref\n{section2}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert!(doc.objects_unlisted_from(body).is_empty(), "{:?}", doc.objects_unlisted_from(body));
    assert_eq!(doc.xref_entry(3), Some(XrefEntry::InFile { offset: body2 as u64, generation: 0 }));
}

/// A hybrid file's cross-reference stream that is unreadable partway keeps what it recorded
/// before the error: here a row /Index declares that the data does not hold, then a pair with a
/// negative count. The stream itself is not used, but the record stays.
#[test]
fn a_hybrid_stream_unreadable_partway_keeps_what_it_recorded() {
    let (base, prev, _, _) = base();
    let doc = Document::open(Arc::new(base.clone())).unwrap();
    let at = |num| match doc.xref_entry(num) {
        Some(XrefEntry::InFile { offset, .. }) => offset,
        other => panic!("object {num} is in the file, not {other:?}"),
    };
    let row = |offset: u64| [1, (offset >> 8) as u8, offset as u8, 0];
    let mut data = vec![0, 0, 0, 255];
    data.extend(row(at(1)));
    data.extend(row(at(2)));
    let mut bytes = base;
    let stream = bytes.len();
    bytes.extend_from_slice(format!("8 0 obj\n<< /Type /XRef /Size 9 /W [1 2 1] /Index [0 10 5 -1] /Length {} >>\nstream\n", data.len()).as_bytes());
    bytes.extend_from_slice(&data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let table = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 0\ntrailer\n<< /Size 9 /Root 1 0 R /XRefStm {stream} /Prev {prev} >>\nstartxref\n{table}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert_eq!(doc.xref_oddities(), [(table as u64, 3)]);
}
