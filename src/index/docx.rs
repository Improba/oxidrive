//! DOCX → Markdown via `word/document.xml` text runs.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use crate::error::OxidriveError;

const MAX_DOCX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DOCX_XML_COMPRESSED_BYTES: u64 = 8 * 1024 * 1024;

/// Extracts plain text from a `.docx`, joining `<w:t>` runs with newlines between paragraphs.
pub fn docx_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let file = File::open(path).map_err(|e| OxidriveError::other(format!("open docx: {e}")))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| OxidriveError::other(format!("zip: {e}")))?;
    let xml_file = archive
        .by_name("word/document.xml")
        .map_err(|e| OxidriveError::other(format!("docx missing document.xml: {e}")))?;
    if xml_file.size() > MAX_DOCX_XML_BYTES
        || xml_file.compressed_size() > MAX_DOCX_XML_COMPRESSED_BYTES
    {
        return Err(OxidriveError::other(format!(
            "document.xml too large for safe indexing (size={}, compressed={})",
            xml_file.size(),
            xml_file.compressed_size()
        )));
    }
    let mut limited = xml_file.take(MAX_DOCX_XML_BYTES + 1);
    let mut xml = String::new();
    limited
        .read_to_string(&mut xml)
        .map_err(|e| OxidriveError::other(format!("read document.xml: {e}")))?;
    if (xml.len() as u64) > MAX_DOCX_XML_BYTES {
        return Err(OxidriveError::other(
            "document.xml exceeded safe read limit",
        ));
    }

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut out = String::new();
    let mut in_text = false;
    let mut buf = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| OxidriveError::other(format!("xml: {e}")))?
        {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if e.name().as_ref() == b"w:p" && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if e.name().as_ref() == b"w:t" {
                    in_text = true;
                }
            }
            Event::Text(e) => {
                if in_text {
                    let t = e
                        .unescape()
                        .map_err(|e| OxidriveError::other(format!("unescape: {e}")))?;
                    out.push_str(&t);
                }
            }
            Event::End(ref e) => {
                if e.name().as_ref() == b"w:t" {
                    in_text = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        Ok(String::from("(empty document)"))
    } else {
        Ok(trimmed)
    }
}
