//! XLSX → Markdown tables (first worksheet, best-effort).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use crate::error::OxidriveError;

const MAX_XLSX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_XLSX_COMPRESSED_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SHARED_STRINGS: usize = 200_000;

fn read_zip_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String, OxidriveError> {
    let f = archive
        .by_name(name)
        .map_err(|_| OxidriveError::other(format!("xlsx missing {name}")))?;
    if f.size() > MAX_XLSX_ENTRY_BYTES || f.compressed_size() > MAX_XLSX_COMPRESSED_ENTRY_BYTES {
        return Err(OxidriveError::other(format!(
            "xlsx entry {name} too large for safe indexing (size={}, compressed={})",
            f.size(),
            f.compressed_size()
        )));
    }
    let mut limited = f.take(MAX_XLSX_ENTRY_BYTES + 1);
    let mut s = String::new();
    limited
        .read_to_string(&mut s)
        .map_err(|e| OxidriveError::other(format!("read {name}: {e}")))?;
    if (s.len() as u64) > MAX_XLSX_ENTRY_BYTES {
        return Err(OxidriveError::other(format!(
            "xlsx entry {name} exceeded safe read limit"
        )));
    }
    Ok(s)
}

/// Parses `sharedStrings.xml` into a vector of shared string values.
fn parse_shared_strings(xml: &str) -> Result<Vec<String>, OxidriveError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut strings = Vec::new();
    let mut buf = Vec::new();
    let mut current = String::new();
    let mut in_si = false;
    let mut in_t = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| OxidriveError::other(format!("sharedStrings xml: {e}")))?
        {
            Event::Start(ref e) => match e.name().as_ref() {
                b"si" => {
                    in_si = true;
                    current.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Event::Text(e) if in_t => {
                let t = e
                    .unescape()
                    .map_err(|e| OxidriveError::other(format!("unescape: {e}")))?;
                current.push_str(&t);
            }
            Event::End(ref e) => match e.name().as_ref() {
                b"t" => in_t = false,
                b"si" => {
                    in_si = false;
                    strings.push(current.clone());
                    if strings.len() > MAX_SHARED_STRINGS {
                        return Err(OxidriveError::other(
                            "sharedStrings exceeded safe indexing limit",
                        ));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(strings)
}

/// Converts the first worksheet to a Markdown table (row/column order as stored in XML).
pub fn xlsx_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let file = File::open(path).map_err(|e| OxidriveError::other(format!("open xlsx: {e}")))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| OxidriveError::other(format!("zip: {e}")))?;

    let shared = read_zip_entry(&mut archive, "xl/sharedStrings.xml")
        .ok()
        .map(|s| parse_shared_strings(&s))
        .transpose()?
        .unwrap_or_default();

    let sheet = read_zip_entry(&mut archive, "xl/worksheets/sheet1.xml")?;
    let mut reader = Reader::from_str(&sheet);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut in_row = false;
    let mut in_v = false;
    let mut cell_buf = String::new();
    let mut cell_type: Option<String> = None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| OxidriveError::other(format!("sheet xml: {e}")))?
        {
            Event::Start(ref e) => {
                if e.name().as_ref() == b"row" {
                    in_row = true;
                    row.clear();
                }
                if in_row && e.name().as_ref() == b"c" {
                    cell_buf.clear();
                    cell_type = e
                        .attributes()
                        .filter_map(|a| a.ok())
                        .find(|a| a.key.as_ref() == b"t")
                        .and_then(|a| String::from_utf8(a.value.into_owned().to_vec()).ok());
                }
                if e.name().as_ref() == b"v" {
                    in_v = true;
                }
            }
            Event::Text(e) if in_v => {
                let t = e
                    .unescape()
                    .map_err(|e| OxidriveError::other(format!("unescape cell: {e}")))?;
                cell_buf.push_str(&t);
            }
            Event::End(ref e) => {
                if e.name().as_ref() == b"v" {
                    in_v = false;
                }
                if e.name().as_ref() == b"c" && in_row {
                    let val = if cell_type.as_deref() == Some("s") {
                        cell_buf
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i).cloned())
                            .unwrap_or_default()
                    } else {
                        cell_buf.clone()
                    };
                    row.push(val);
                }
                if e.name().as_ref() == b"row" {
                    in_row = false;
                    if !row.is_empty() {
                        rows.push(row.clone());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if rows.is_empty() {
        return Ok("(empty sheet)".into());
    }

    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut md = String::new();
    for (i, r) in rows.iter().enumerate() {
        let mut line = String::from("|");
        for c in 0..ncols {
            let cell = r.get(c).cloned().unwrap_or_default().replace('|', "\\|");
            line.push_str(&format!(" {cell} |"));
        }
        md.push_str(&line);
        md.push('\n');
        if i == 0 {
            line = String::from("|");
            for _ in 0..ncols {
                line.push_str(" --- |");
            }
            md.push_str(&line);
            md.push('\n');
        }
    }
    Ok(md)
}
