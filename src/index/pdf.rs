//! PDF → Markdown via `pdf-extract`.

use std::path::Path;

use crate::error::OxidriveError;

const MAX_PDF_INDEX_BYTES: u64 = 64 * 1024 * 1024;

/// Extracts text content from a PDF file and returns it as Markdown.
/// Best-effort: some PDFs (scanned images, encrypted) will return minimal content.
pub fn pdf_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        OxidriveError::other(format!(
            "Failed to stat PDF for indexing {}: {e}",
            path.display()
        ))
    })?;
    if metadata.len() > MAX_PDF_INDEX_BYTES {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        return Ok(format!(
            "# {filename}\n\n*(PDF trop volumineux pour une extraction sûre: {} octets, limite {} octets)*\n",
            metadata.len(),
            MAX_PDF_INDEX_BYTES
        ));
    }

    let text = pdf_extract::extract_text(path).map_err(|e| {
        OxidriveError::other(format!(
            "PDF text extraction failed for {}: {e}",
            path.display()
        ))
    })?;

    let text = text.trim();
    if text.is_empty() {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        return Ok(format!(
            "# {filename}\n\n*(PDF sans texte extractible — probablement un scan ou un document image)*\n"
        ));
    }

    // Add a title from the filename
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(format!("# {filename}\n\n{text}\n"))
}
