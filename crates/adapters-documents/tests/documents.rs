//! The document reader against real files built here, and against files
//! built to break it (ADR 0062).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good fixtures; a panic is the failure report"
)]

use std::io::Write;

use tramontana_adapters_documents::{Documents, PDF, WORD, WORD_XML_BYTES};
use tramontana_core::ports::{DocumentError, DocumentReader};

mod common;
use common::{body, pdf, word};

#[test]
fn it_says_which_names_it_reads() {
    let reader = Documents::new();
    assert_eq!(reader.media_type("Contract 2026.PDF"), Some(PDF));
    assert_eq!(reader.media_type("minutes.docx"), Some(WORD));
    assert_eq!(reader.media_type("old.doc"), None);
    assert_eq!(reader.media_type("notes.txt"), None);
}

#[test]
fn a_pdf_s_text_is_read_page_by_page() {
    let text = Documents::new()
        .read_text(
            "agreement.pdf",
            &pdf(&["Supplier agreement", "Payment within 30 days"]),
        )
        .unwrap();
    // Each page its own paragraph, in order, never run together.
    assert_eq!(text, "Supplier agreement\n\nPayment within 30 days");
}

#[test]
fn a_word_document_s_paragraphs_tabs_breaks_and_entities_are_read() {
    let document = body(
        r#"<w:p><w:r><w:t>Minutes &amp; actions</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Owner:</w:t></w:r><w:r><w:tab/><w:t>Ana</w:t></w:r></w:p>
<w:p><w:r><w:t>Line one</w:t><w:br/><w:t>line two &#8211; done</w:t></w:r></w:p>"#,
    );
    let text = Documents::new()
        .read_text("minutes.docx", &word(&document))
        .unwrap();
    assert_eq!(
        text,
        "Minutes & actions\nOwner:\tAna\nLine one\nline two – done"
    );
}

#[test]
fn what_is_not_the_document_it_claims_to_be_is_unreadable_not_a_crash() {
    let reader = Documents::new();
    for (name, bytes) in [
        (
            "broken.pdf",
            b"%PDF-1.4\n1 0 obj << /Type /Catalog".to_vec(),
        ),
        ("not-a.pdf", b"PK\x03\x04 a zip, not a pdf".to_vec()),
        ("broken.docx", b"PK\x03\x04 truncated".to_vec()),
        ("text.docx", b"just some text".to_vec()),
        ("no-part.docx", {
            let mut buffer = std::io::Cursor::new(Vec::new());
            let mut zip = zip::ZipWriter::new(&mut buffer);
            zip.start_file("other.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"<x/>").unwrap();
            zip.finish().unwrap();
            buffer.into_inner()
        }),
        (
            "bad-xml.docx",
            word("<w:document><w:body><w:p><w:t>unclosed"),
        ),
    ] {
        match reader.read_text(name, &bytes) {
            Err(DocumentError::Unreadable { .. }) => {}
            other => panic!("{name}: expected unreadable, got {other:?}"),
        }
    }
    // Arbitrary bytes that start like a PDF must not take the process down.
    let mut noise = b"%PDF-1.7\n".to_vec();
    noise.extend((0..20_000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 24) as u8));
    assert!(reader.read_text("noise.pdf", &noise).is_err());
}

#[test]
fn a_word_part_that_inflates_past_the_limit_is_refused_unread() {
    let limit = usize::try_from(WORD_XML_BYTES).unwrap();
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("word/document.xml", deflated).unwrap();
        let chunk = vec![b' '; 1024 * 1024];
        for _ in 0..=(limit / chunk.len()) {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
    }
    let bytes = buffer.into_inner();
    assert!(bytes.len() < 1024 * 1024, "the fixture is a small file");
    assert!(matches!(
        Documents::new().read_text("bomb.docx", &bytes),
        Err(DocumentError::TooLarge { .. })
    ));
}
