//! Real-PDF test for `select_text_in_rect` and `select_text_range`: a multi-line drag must
//! produce one quad per line and the actual selected text, not the drag rectangle's own
//! bounding box, and a line-aware range selection must pull in whole middle lines even when
//! the drag itself is a straight vertical line. Requires PDFium (skips otherwise, like the
//! other real-PDF tests in this crate).

use fond_doc::{bind_pdfium, select_text_in_rect, select_text_range};

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
    assert!(
        selection.text.contains("first"),
        "got: {:?}",
        selection.text
    );
    assert!(
        selection.text.contains("second"),
        "got: {:?}",
        selection.text
    );
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
    assert!(
        selection.text.contains("first"),
        "got: {:?}",
        selection.text
    );
    assert!(
        !selection.text.contains("second"),
        "got: {:?}",
        selection.text
    );
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

fn three_line_pdf() -> Vec<u8> {
    let content = "BT /F1 18 Tf 72 700 Td (The first line of text) Tj ET \
                    BT /F1 18 Tf 72 660 Td (A middle line in between) Tj ET \
                    BT /F1 18 Tf 72 620 Td (The third and final line) Tj ET";
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
fn straight_vertical_drag_selects_whole_middle_lines() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = three_line_pdf();

    // A perfectly vertical drag (same x for both endpoints) starting above line one and
    // ending below line three — the failure mode this guards against is a plain
    // rectangle-intersection selection, which would only pick up the narrow column of text
    // directly under x=100 on every line instead of pulling in whole in-between lines.
    let selection = select_text_range(&pdfium, &pdf, 0, 100.0, 725.0, 100.0, 615.0)
        .unwrap()
        .expect("expected a selection spanning all three lines");

    assert_eq!(selection.quads.len(), 3, "one quad per line: {selection:?}");
    // The first line is trimmed to keep only text at/after the drag's x, and the last line
    // to keep only text at/before it (same x for both, since this is a straight vertical
    // drag) — the middle line gets neither trim, so it's the one guarantee this test can make
    // about *content*: full text present regardless of the drag's x position. Exactly where
    // the first/last line's trim boundary falls depends on the substituted font's actual
    // glyph widths (this PDF references "Helvetica" by name, not embedded), which varies
    // across PDFium builds/versions — asserting a specific word survives that boundary on the
    // first or last line is asserting on glyph metrics, not on select_text_range's logic, and
    // was flaky here for exactly that reason (passed against a system-installed PDFium,
    // failed against the newer build CI's own "fetch latest PDFium" step downloads).
    assert!(
        selection.text.contains("middle line in between"),
        "middle line should be selected in full: {:?}",
        selection.text
    );
}

#[test]
fn range_selection_direction_does_not_matter() {
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: PDFium not available ({e})");
            return;
        }
    };
    let pdf = three_line_pdf();

    let forward = select_text_range(&pdfium, &pdf, 0, 100.0, 725.0, 100.0, 615.0)
        .unwrap()
        .expect("forward drag should select text");
    let backward = select_text_range(&pdfium, &pdf, 0, 100.0, 615.0, 100.0, 725.0)
        .unwrap()
        .expect("a drag dragged bottom-to-top should select the same text");

    assert_eq!(forward.text, backward.text);
}
