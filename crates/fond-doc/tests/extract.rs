//! Real PDF text-extraction test. Requires a PDFium native library: set `PDFIUM_LIB_PATH`
//! to a directory containing `libpdfium.so` (or the file). Without it, the test skips
//! rather than fails, so CI on a machine without PDFium stays green.

use fond_doc::{bind_pdfium, extract_text};

/// Build a minimal single-page PDF containing `text`, with a correct cross-reference
/// table (offsets computed here so PDFium parses it without invoking its repair path).
fn minimal_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, obj));
    }

    let xref_offset = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for off in &offsets {
        pdf.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        objects.len() + 1,
        xref_offset
    ));
    pdf.into_bytes()
}

#[test]
fn extracts_text_layer_from_a_pdf() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e}). Set PDFIUM_LIB_PATH to run this test.");
            return;
        }
    };

    let pdf = minimal_pdf("Kartoteka liberation theology");
    let extracted = extract_text(&pdfium, &pdf).unwrap();

    assert_eq!(extracted.page_count, 1);
    let text = extracted.full_text();
    assert!(
        text.contains("Kartoteka"),
        "expected extracted text to contain 'Kartoteka', got: {text:?}"
    );
    assert!(text.contains("liberation"), "got: {text:?}");
}
