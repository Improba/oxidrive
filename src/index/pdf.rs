//! PDF → Markdown via `pdf-extract`.

use std::path::Path;

use crate::error::OxidriveError;

/// Extracts text content from a PDF file and returns it as Markdown.
/// Best-effort: some PDFs (scanned images, encrypted) will return minimal content.
pub fn pdf_to_markdown(path: &Path) -> Result<String, OxidriveError> {
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
