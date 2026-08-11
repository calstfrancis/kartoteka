//! Real-PDF test for `select_text_in_rect`: a multi-line drag must produce one quad per
//! line and the actual selected text, not the drag rectangle's own bounding box. Requires
//! PDFium (skips otherwise, like the other real-PDF tests in this crate).

use fond_doc::{bind_pdfium, select_text_in_rect};

fn two_line_pdf() -> Vec<u8> {
    let content = "BT /F1 18 Tf 72 700 Td (The first line of text) Tj ET \
                    BT /F1 18 Tf 72 660 Td (The second line follows) Tj ET";
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
fn selects_text_spanning_two_lines_as_two_quads() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = two_line_pdf();

    // A rectangle covering both lines' full extent.
    let selection = select_text_in_rect(&pdfium, &pdf, 0, 60.0, 655.0, 560.0, 725.0)
        .unwrap()
        .expect("expected a selection covering both lines");

    assert_eq!(selection.quads.len(), 2, "one quad per line: {selection:?}");
    assert!(selection.text.contains("first"), "got: {:?}", selection.text);
    assert!(selection.text.contains("second"), "got: {:?}", selection.text);
}

#[test]
fn selects_text_within_a_single_line_only() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = two_line_pdf();

    // A rectangle over the first line's band only.
    let selection = select_text_in_rect(&pdfium, &pdf, 0, 60.0, 695.0, 560.0, 725.0)
        .unwrap()
        .expect("expected a selection covering the first line");

    assert_eq!(selection.quads.len(), 1, "one line only: {selection:?}");
    assert!(selection.text.contains("first"), "got: {:?}", selection.text);
    assert!(!selection.text.contains("second"), "got: {:?}", selection.text);
}

#[test]
fn returns_none_over_a_blank_area() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = two_line_pdf();

    let selection = select_text_in_rect(&pdfium, &pdf, 0, 60.0, 100.0, 200.0, 200.0).unwrap();
    assert!(selection.is_none(), "expected no text in a blank margin");
}
