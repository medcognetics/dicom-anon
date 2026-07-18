#![cfg(feature = "pdf")]

use std::path::PathBuf;

use dicom_anon_report::{
    Category, DetectedSpan, PdfAnonymizer, PdfOptions, PdfTextMode, PrivacyDetector, ReportOptions,
    Result,
};

struct NameDetector;

impl PrivacyDetector for NameDetector {
    fn detect(&self, text: &str) -> Result<Vec<DetectedSpan>> {
        let Some(start) = text.find("Alice Smith") else {
            return Ok(Vec::new());
        };
        Ok(vec![DetectedSpan::new(
            Category::PrivatePerson,
            start,
            start + "Alice Smith".len(),
            0.99,
        )?])
    }
}

#[test]
#[ignore = "requires a dynamically loadable PDFium library"]
fn embedded_text_pdf_is_rebuilt_as_image_only_output() {
    let pdfium_library = PathBuf::from(std::env::var("DICOM_ANON_REPORT_PDFIUM_LIBRARY").unwrap());
    let input = minimal_text_pdf("Patient Alice Smith has a normal exam.");
    let anonymizer = PdfAnonymizer::new(
        NameDetector,
        ReportOptions::default(),
        PdfOptions {
            pdfium_library,
            text_mode: PdfTextMode::Embedded,
            ..PdfOptions::default()
        },
    );

    let output = anonymizer.anonymize_pdf(&input).unwrap();

    assert!(output.bytes.starts_with(b"%PDF-"));
    assert!(!output
        .bytes
        .windows("Alice Smith".len())
        .any(|window| window == b"Alice Smith"));
    assert_eq!(output.report.pages, 1);
    assert_eq!(output.report.embedded_text_pages, 1);
    assert_eq!(output.report.ocr_pages, 0);
    assert!(output.report.image_only_output);
    assert_eq!(output.report.privacy.detected_spans, 1);
}

#[test]
#[ignore = "requires PDFium, Tesseract, Leptonica, and English OCR data"]
fn forced_ocr_pdf_is_rebuilt_as_image_only_output() {
    let pdfium_library = PathBuf::from(std::env::var("DICOM_ANON_REPORT_PDFIUM_LIBRARY").unwrap());
    let input = minimal_text_pdf("Patient Alice Smith has a normal exam.");
    let anonymizer = PdfAnonymizer::new(
        NameDetector,
        ReportOptions::default(),
        PdfOptions {
            pdfium_library,
            dpi: 300,
            text_mode: PdfTextMode::Ocr,
            ..PdfOptions::default()
        },
    );

    let output = anonymizer.anonymize_pdf(&input).unwrap();

    assert!(output.bytes.starts_with(b"%PDF-"));
    assert_eq!(output.report.embedded_text_pages, 0);
    assert_eq!(output.report.ocr_pages, 1);
    assert_eq!(output.report.privacy.detected_spans, 1);
}

#[test]
#[ignore = "requires PDFium, Tesseract, Leptonica, and English OCR data"]
fn auto_mode_checks_both_embedded_text_and_rendered_pixels() {
    let pdfium_library = PathBuf::from(std::env::var("DICOM_ANON_REPORT_PDFIUM_LIBRARY").unwrap());
    let input = minimal_text_pdf("Patient Alice Smith has a normal exam.");
    let anonymizer = PdfAnonymizer::new(
        NameDetector,
        ReportOptions::default(),
        PdfOptions {
            pdfium_library,
            dpi: 300,
            text_mode: PdfTextMode::Auto,
            ..PdfOptions::default()
        },
    );

    let output = anonymizer.anonymize_pdf(&input).unwrap();

    assert_eq!(output.report.embedded_text_pages, 1);
    assert_eq!(output.report.ocr_pages, 1);
    assert_eq!(output.report.privacy.detected_spans, 2);
}

fn minimal_text_pdf(text: &str) -> Vec<u8> {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let stream = format!("BT /F1 18 Tf 72 720 Td ({escaped}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
