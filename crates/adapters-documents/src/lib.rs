//! Text from documents a person attaches: PDF and Word (ADR 0062).
//!
//! Given bytes, returns text. Touches no file, network or process. Core
//! decides what may be attached, checks sizes before calling, and redacts
//! what comes back; this only reads.
//!
//! - **Word** (`.docx`) is read here: the document part of its zip container,
//!   through `quick-xml`, taking paragraph text, tabs and line breaks. Only
//!   that part is unpacked, and never more than [`WORD_XML_BYTES`] of it, so a
//!   container that inflates without end is refused, not followed.
//! - **PDF** text comes from `pdf-extract`. A malformed file must not take the
//!   application down with it, so a panic inside the library is caught and
//!   reported as an unreadable document.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};

use quick_xml::events::Event;
use tramontana_core::ports::{DocumentError, DocumentReader};

/// The media type of a PDF.
pub const PDF: &str = "application/pdf";

/// The media type of a Word document.
pub const WORD: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// The most a Word document's text part is unpacked to.
pub const WORD_XML_BYTES: u64 = 32 * 1024 * 1024;

/// The most entries a Word container is looked through.
const WORD_ENTRIES: usize = 10_000;

/// Reads PDF and Word documents.
#[derive(Debug, Default, Clone, Copy)]
pub struct Documents;

impl Documents {
    /// A reader for both kinds.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn extension(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
}

impl DocumentReader for Documents {
    fn media_type(&self, name: &str) -> Option<&'static str> {
        match extension(name).as_deref() {
            Some("pdf") => Some(PDF),
            Some("docx") => Some(WORD),
            _ => None,
        }
    }

    fn read_text(&self, name: &str, bytes: &[u8]) -> Result<String, DocumentError> {
        match self.media_type(name) {
            Some(PDF) => read_pdf(bytes),
            Some(WORD) => read_word(bytes),
            _ => Err(DocumentError::Unreadable { kind: "supported" }),
        }
    }
}

/// Collapse runs of blank lines and trailing spaces, which extraction leaves
/// in abundance, so the text sent is the text.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0usize;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_owned()
}

fn read_pdf(bytes: &[u8]) -> Result<String, DocumentError> {
    const KIND: &str = "PDF";
    if !bytes.starts_with(b"%PDF-") {
        return Err(DocumentError::Unreadable { kind: KIND });
    }
    let extracted = catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem_by_pages(bytes)
    }))
    .map_err(|_| DocumentError::Unreadable { kind: KIND })?;
    match extracted {
        // Page by page, with a blank line between pages: read whole, a
        // page's last line and the next page's first run together.
        Ok(pages) => Ok(tidy(
            &pages
                .iter()
                .map(|page| page.trim())
                .filter(|page| !page.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        )),
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            if message.contains("encrypt") || message.contains("password") {
                Err(DocumentError::Encrypted { kind: KIND })
            } else {
                Err(DocumentError::Unreadable { kind: KIND })
            }
        }
    }
}

fn read_word(bytes: &[u8]) -> Result<String, DocumentError> {
    const KIND: &str = "Word";
    let unreadable = || DocumentError::Unreadable { kind: KIND };
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| unreadable())?;
    if archive.len() > WORD_ENTRIES {
        return Err(DocumentError::TooLarge { kind: KIND });
    }
    let mut part = match archive.by_name("word/document.xml") {
        Ok(part) => part,
        Err(zip::result::ZipError::UnsupportedArchive(message))
            if message.to_ascii_lowercase().contains("password") =>
        {
            return Err(DocumentError::Encrypted { kind: KIND });
        }
        Err(_) => return Err(unreadable()),
    };
    if part.size() > WORD_XML_BYTES {
        return Err(DocumentError::TooLarge { kind: KIND });
    }
    // The size in the header is the archive's claim; the read is bounded
    // whatever it says.
    let mut xml = Vec::new();
    (&mut part)
        .take(WORD_XML_BYTES + 1)
        .read_to_end(&mut xml)
        .map_err(|_| unreadable())?;
    if xml.len() as u64 > WORD_XML_BYTES {
        return Err(DocumentError::TooLarge { kind: KIND });
    }
    word_text(&xml)
        .map(|text| tidy(&text))
        .ok_or_else(unreadable)
}

/// The text of a Word document part: each paragraph a line, with its tabs
/// and breaks. `None` when the XML is not well formed.
fn word_text(xml: &[u8]) -> Option<String> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut text = String::new();
    let mut in_text = false;
    // A document that ends inside an element was cut short; half of it is
    // not what the person attached.
    let mut depth = 0usize;
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(element) => {
                depth += 1;
                if element.local_name().into_inner() == "t" {
                    in_text = true;
                }
            }
            Event::Empty(element) => match element.local_name().into_inner() {
                "tab" => text.push('\t'),
                "br" | "cr" => text.push('\n'),
                "p" => text.push('\n'),
                _ => {}
            },
            Event::End(element) => {
                depth = depth.checked_sub(1)?;
                match element.local_name().into_inner() {
                    "t" => in_text = false,
                    "p" => text.push('\n'),
                    _ => {}
                }
            }
            Event::Text(content) if in_text => {
                text.push_str(&content.into_inner());
            }
            Event::GeneralRef(reference) if in_text => {
                let name = reference.into_inner();
                let resolved = match name.as_ref() {
                    "amp" => "&".to_owned(),
                    "lt" => "<".to_owned(),
                    "gt" => ">".to_owned(),
                    "quot" => "\"".to_owned(),
                    "apos" => "'".to_owned(),
                    other => other
                        .strip_prefix('#')
                        .and_then(|number| {
                            number.strip_prefix('x').map_or_else(
                                || number.parse::<u32>().ok(),
                                |hex| u32::from_str_radix(hex, 16).ok(),
                            )
                        })
                        .and_then(char::from_u32)
                        .map(String::from)?,
                };
                text.push_str(&resolved);
            }
            Event::Eof if depth == 0 => break,
            Event::Eof => return None,
            _ => {}
        }
        buffer.clear();
    }
    Some(text)
}
