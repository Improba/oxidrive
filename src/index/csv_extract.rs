//! CSV → Markdown table.

use std::fs::File;
use std::path::Path;

use csv::ReaderBuilder;

use crate::error::OxidriveError;

/// Renders the first rows of a CSV file as a GitHub-flavored Markdown table.
pub fn csv_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let f = File::open(path).map_err(|e| OxidriveError::other(format!("open csv: {e}")))?;
    let mut rdr = ReaderBuilder::new().has_headers(false).from_reader(f);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| OxidriveError::other(format!("csv parse: {e}")))?;
        rows.push(rec.iter().map(|s| s.to_string()).collect());
        if rows.len() > 500 {
            break;
        }
    }

    if rows.is_empty() {
        return Ok("(empty csv)".into());
    }

    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut md = String::new();
    for (i, row) in rows.iter().enumerate() {
        md.push('|');
        for c in 0..ncols {
            let cell = row.get(c).cloned().unwrap_or_default().replace('|', "\\|");
            md.push_str(&format!(" {cell} |"));
        }
        md.push('\n');
        if i == 0 {
            md.push('|');
            for _ in 0..ncols {
                md.push_str(" --- |");
            }
            md.push('\n');
        }
    }
    Ok(md)
}
