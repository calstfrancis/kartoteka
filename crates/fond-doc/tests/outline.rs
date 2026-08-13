//! Real-PDF test for `outline()`: a hand-built two-page PDF with a nested `/Outlines` tree,
//! read back and checked for document order, depth, and page resolution. Requires PDFium
//! (skips otherwise, like the other real-PDF tests in this crate).

use fond_doc::{bind_pdfium, outline};

/// A two-page PDF with an outline tree: a top-level "Chapter One" bookmark pointing at page
/// 1, with a nested child "Section 1.1" pointing at page 2.
fn pdf_with_outline() -> Vec<u8> {
    let content1 = "BT /F1 24 Tf 72 700 Td (Page one text) Tj ET";
    let content2 = "BT /F1 24 Tf 72 700 Td (Page two text) Tj ET";
    let objects = [
        // 1: Catalog
        "<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>".to_string(),
        // 2: Pages
        "<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_string(),
        // 3: Page 1
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        // 4: Contents 1
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content1.len(),
            content1
        ),
        // 5: Font
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        // 6: Outlines root
        "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>".to_string(),
        // 7: Page 2
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 10 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        // 8: Outline item "Chapter One" (child: 9)
        "<< /Title (Chapter One) /Parent 6 0 R /Dest [3 0 R /Fit] \
         /First 9 0 R /Last 9 0 R /Count 1 >>"
            .to_string(),
        // 9: Outline item "Section 1.1" (parent: 8)
        "<< /Title (Section 1.1) /Parent 8 0 R /Dest [7 0 R /Fit] >>".to_string(),
        // 10: Contents 2
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content2.len(),
            content2
        ),
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
fn outline_reads_titles_depth_and_page_in_document_order() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let pdf = pdf_with_outline();
    let entries = outline(&pdfium, &pdf).unwrap();

    assert_eq!(
        entries.len(),
        2,
        "expected two outline entries: {entries:?}"
    );

    assert_eq!(entries[0].title, "Chapter One");
    assert_eq!(entries[0].depth, 0);
    assert_eq!(entries[0].page, Some(1));

    assert_eq!(entries[1].title, "Section 1.1");
    assert_eq!(
        entries[1].depth, 1,
        "nested bookmark should be one level deep"
    );
    assert_eq!(entries[1].page, Some(2));
}

#[test]
fn outline_is_empty_for_a_pdf_with_no_outlines() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    // Reuse the plain single-page fixture shape (no /Outlines key at all in the catalog).
    let content = "BT /F1 24 Tf 72 700 Td (no outline here) Tj ET";
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

    let entries = outline(&pdfium, &pdf.into_bytes()).unwrap();
    assert!(entries.is_empty());
}
