//! Round-trip test for embedded annotations: write a highlight into a PDF, read it back.
//! Requires PDFium (skips otherwise, like the extraction test).

use std::sync::Mutex;

use fond_doc::{bind_pdfium, embed_highlights, extract_annotations, AnnotationToEmbed};

/// PDFium is not safe to drive from two test threads at once (two tests in this binary running
/// in parallel abort with "double free or corruption"), so tests that use it hold this.
static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

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
    let xref = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in &offsets {
        pdf.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        objects.len() + 1,
        xref
    ));
    pdf.into_bytes()
}

#[test]
fn embed_then_extract_round_trips_a_highlight() {
    let _guard = PDFIUM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let pdf = minimal_pdf("highlight me please");
    let to_embed = AnnotationToEmbed {
        page: 1,
        // A quad covering the text line (x 72..500, y 698..726).
        quadpoints: vec![[72.0, 726.0, 500.0, 726.0, 72.0, 698.0, 500.0, 698.0]],
        contents: Some("core claim".to_string()),
    };

    let annotated = embed_highlights(pdfium, &pdf, std::slice::from_ref(&to_embed)).unwrap();
    let extracted = extract_annotations(pdfium, &annotated).unwrap();

    assert_eq!(extracted.len(), 1, "expected one annotation: {extracted:?}");
    let a = &extracted[0];
    assert_eq!(a.page, 1);
    assert_eq!(a.contents.as_deref(), Some("core claim"));
    assert!(!a.quadpoints.is_empty());
    // The highlighted text should be recoverable from the text layer.
    if let Some(snippet) = &a.snippet {
        assert!(snippet.contains("highlight"), "snippet was: {snippet:?}");
    }
}

/// A highlight spanning two lines must come back as two quads, not one bounding box that
/// covers the gap (and whatever is in it) on export.
#[test]
fn multi_line_highlight_keeps_one_quad_per_line() {
    let _guard = PDFIUM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = minimal_pdf("two line highlight");
    let to_embed = AnnotationToEmbed {
        page: 1,
        quadpoints: vec![
            [72.0, 726.0, 500.0, 726.0, 72.0, 698.0, 500.0, 698.0],
            [72.0, 660.0, 300.0, 660.0, 72.0, 632.0, 300.0, 632.0],
        ],
        contents: None,
    };
    let annotated = embed_highlights(pdfium, &pdf, std::slice::from_ref(&to_embed)).unwrap();
    let extracted = extract_annotations(pdfium, &annotated).unwrap();
    assert_eq!(extracted.len(), 1);
    assert_eq!(
        extracted[0].quadpoints.len(),
        2,
        "quads collapsed: {:?}",
        extracted[0].quadpoints
    );
}
