//! Real-PDF test for `page_labels()`: a hand-built four-page PDF whose `/PageLabels`
//! declares lowercase-roman front matter (pages 1-2 print as "i"/"ii") followed by arabic
//! body pages restarting at 1 (pages 3-4 print as "1"/"2") — the exact case Zotero-style
//! document-page-number display exists for: the printed number often isn't the raw file
//! position. Requires PDFium (skips otherwise, like the other real-PDF tests in this crate).

use fond_doc::{bind_pdfium, page_labels};

fn pdf_with_page_labels() -> Vec<u8> {
    let page_content = |text: &str| {
        let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        )
    };
    let objects = [
        // 1: Catalog — references the page tree and the page-labels number tree.
        "<< /Type /Catalog /Pages 2 0 R /PageLabels 8 0 R >>".to_string(),
        // 2: Pages
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>".to_string(),
        // 3-6: four plain pages, sharing one font and their own content streams.
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>"
            .to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 10 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>"
            .to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 11 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>"
            .to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 12 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>"
            .to_string(),
        // 7: Font
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        // 8: PageLabels number tree — index 0 (page 1): lowercase roman starting at 1;
        // index 2 (page 3): decimal starting at 1.
        "<< /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >>".to_string(),
        // 9-12: content streams for the four pages.
        page_content("Front matter one"),
        page_content("Front matter two"),
        page_content("Body one"),
        page_content("Body two"),
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
fn reads_mixed_roman_and_restarted_arabic_labels_in_document_order() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let pdf = pdf_with_page_labels();
    let labels = page_labels(&pdfium, &pdf).unwrap();

    assert_eq!(labels.len(), 4, "expected four labels: {labels:?}");
    assert_eq!(labels[0].as_deref(), Some("i"));
    assert_eq!(labels[1].as_deref(), Some("ii"));
    // The arabic range restarts at 1, matching /St 1 — the whole reason this isn't just the
    // raw 1-based page position (raw pages 3 and 4 print as "1" and "2").
    assert_eq!(labels[2].as_deref(), Some("1"));
    assert_eq!(labels[3].as_deref(), Some("2"));
}

#[test]
fn is_none_for_every_page_when_the_pdf_has_no_page_labels() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };

    let content = "BT /F1 24 Tf 72 700 Td (no page labels here) Tj ET";
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

    let labels = page_labels(&pdfium, &pdf.into_bytes()).unwrap();
    assert_eq!(labels, vec![None]);
}
