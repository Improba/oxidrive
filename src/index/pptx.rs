//! PPTX → Markdown sections (one heading per slide, text from `a:t` runs).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use crate::error::OxidriveError;

const MAX_PPTX_SLIDES: usize = 500;
const MAX_PPTX_SLIDE_XML_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PPTX_SLIDE_XML_COMPRESSED_BYTES: u64 = 4 * 1024 * 1024;

/// Extracts visible text from each `ppt/slides/slideN.xml` file.
pub fn pptx_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let file = File::open(path).map_err(|e| OxidriveError::other(format!("open pptx: {e}")))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| OxidriveError::other(format!("zip: {e}")))?;

    let mut names: Vec<String> = archive
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(String::from)
        .collect();
    names.sort();
    if names.len() > MAX_PPTX_SLIDES {
        names.truncate(MAX_PPTX_SLIDES);
    }

    let mut md = String::new();
    for (i, name) in names.iter().enumerate() {
        let z = archive
            .by_name(name)
            .map_err(|e| OxidriveError::other(format!("pptx read {name}: {e}")))?;
        if z.size() > MAX_PPTX_SLIDE_XML_BYTES
            || z.compressed_size() > MAX_PPTX_SLIDE_XML_COMPRESSED_BYTES
        {
            return Err(OxidriveError::other(format!(
                "pptx slide XML too large for safe indexing (slide={}, size={}, compressed={})",
                i + 1,
                z.size(),
                z.compressed_size()
            )));
        }
        let mut limited = z.take(MAX_PPTX_SLIDE_XML_BYTES + 1);
        let mut xml = String::new();
        limited
            .read_to_string(&mut xml)
            .map_err(|e| OxidriveError::other(format!("read slide: {e}")))?;
        if (xml.len() as u64) > MAX_PPTX_SLIDE_XML_BYTES {
            return Err(OxidriveError::other(format!(
                "pptx slide XML exceeded safe read limit (slide={})",
                i + 1
            )));
        }

        md.push_str(&format!("## Slide {}\n\n", i + 1));
        md.push_str(&slide_xml_to_text(&xml)?);
        md.push('\n');
    }

    if md.trim().is_empty() {
        Ok("(no slides found)".into())
    } else {
        Ok(md)
    }
}

fn slide_xml_to_text(xml: &str) -> Result<String, OxidriveError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = String::new();
    let mut in_t = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| OxidriveError::other(format!("slide xml: {e}")))?
        {
            Event::Start(ref e) if e.name().as_ref() == b"a:t" => in_t = true,
            Event::Text(e) if in_t => {
                let t = e
                    .unescape()
                    .map_err(|e| OxidriveError::other(format!("unescape: {e}")))?;
                out.push_str(&t);
                out.push(' ');
            }
            Event::End(ref e) if e.name().as_ref() == b"a:t" => {
                in_t = false;
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}
