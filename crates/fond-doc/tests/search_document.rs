//! Real-PDF test for `search_document()`: a two-page PDF with the search term appearing once
//! per page, confirming both hits are found in document order with real bounding quads.
//! Requires PDFium (skips otherwise, like the other real-PDF tests in this crate).

use fond_doc::{bind_pdfium, search_document};

fn two_page_pdf() -> Vec<u8> {
    let content1 = "BT /F1 24 Tf 72 700 Td (find the treasure here) Tj ET";
    let content2 = "BT /F1 24 Tf 72 700 Td (no relevant text) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content1.len(),
            content1
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
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
fn finds_a_match_only_on_the_page_that_has_it() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let pdf = two_page_pdf();
    let matches = search_document(&pdfium, &pdf, "treasure").unwrap();

    assert_eq!(matches.len(), 1, "expected one match: {matches:?}");
    assert_eq!(
        matches[0].page, 0,
        "match should be on the first page (0-based)"
    );
    assert!(!matches[0].quads.is_empty());
}

#[test]
fn is_case_insensitive_and_empty_for_no_hits_or_empty_query() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let pdf = two_page_pdf();
    assert_eq!(search_document(&pdfium, &pdf, "TREASURE").unwrap().len(), 1);
    assert_eq!(
        search_document(&pdfium, &pdf, "nonexistent").unwrap().len(),
        0
    );
    assert_eq!(search_document(&pdfium, &pdf, "").unwrap().len(), 0);
    assert_eq!(search_document(&pdfium, &pdf, "   ").unwrap().len(), 0);
}
