//! PDF rendering, text extraction, OCR fallback, and irreversible visual redaction.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use clap::ValueEnum;
use image::{DynamicImage, ImageFormat, Rgba};
use leptess::LepTess;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};

use crate::linkage::SensitiveLinkageMap;
use crate::redact::{PrivacyDetector, RedactionPlan, ReportAnonymizationReport};
use crate::{Error, ReportAnonymizer, ReportOptions, Result};

const PDF_REPORT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_DPI: u16 = 200;
const DEFAULT_MAX_PAGES: u32 = 500;
const DEFAULT_MAX_RENDER_PIXELS: u64 = 100_000_000;
const DEFAULT_MAX_TOTAL_RENDER_PIXELS: u64 = 250_000_000;
const MIN_EMBEDDED_NON_WHITESPACE_CHARACTERS: usize = 8;
const PDF_POINTS_PER_INCH: f32 = 72.0;
const REDACTION_PADDING_PIXELS: i32 = 2;

static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
static PDFIUM_PATH: OnceLock<PathBuf> = OnceLock::new();
static PDFIUM_INIT: Mutex<()> = Mutex::new(());
static PDFIUM_OPERATION: Mutex<()> = Mutex::new(());

/// Text-location strategy for PDF pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum PdfTextMode {
    /// Use embedded text when present and also OCR every rendered page.
    #[default]
    Auto,
    /// Require usable embedded PDF text on every page.
    Embedded,
    /// OCR every rendered page.
    Ocr,
}

/// PDF rendering and OCR configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfOptions {
    /// Exact path to the dynamically loaded PDFium library.
    pub pdfium_library: PathBuf,
    /// Rasterization resolution used for redaction and image-only output.
    pub dpi: u16,
    /// Embedded-text versus OCR selection.
    pub text_mode: PdfTextMode,
    /// Tesseract language identifier, such as `eng`.
    pub ocr_language: String,
    /// Maximum accepted page count.
    pub max_pages: u32,
    /// Maximum rendered pixels accepted for any one page.
    pub max_render_pixels_per_page: u64,
    /// Maximum rendered pixels accepted across the complete document.
    pub max_total_render_pixels: u64,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            pdfium_library: PathBuf::from(Pdfium::pdfium_platform_library_name()),
            dpi: DEFAULT_DPI,
            text_mode: PdfTextMode::Auto,
            ocr_language: "eng".to_owned(),
            max_pages: DEFAULT_MAX_PAGES,
            max_render_pixels_per_page: DEFAULT_MAX_RENDER_PIXELS,
            max_total_render_pixels: DEFAULT_MAX_TOTAL_RENDER_PIXELS,
        }
    }
}

/// Safe summary of PDF processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfAnonymizationReport {
    /// PDF report schema version.
    pub schema_version: u32,
    /// Aggregate privacy-filter report across all pages.
    pub privacy: ReportAnonymizationReport,
    /// Number of source and output pages.
    pub pages: u32,
    /// Pages localized using embedded PDF text.
    pub embedded_text_pages: u32,
    /// Pages localized using Tesseract OCR.
    pub ocr_pages: u32,
    /// Always true: output pages contain raster images and no source text layer.
    pub image_only_output: bool,
}

/// Sanitized image-only PDF bytes and their safe report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonymizedPdf {
    /// Newly constructed PDF bytes.
    pub bytes: Vec<u8>,
    /// Safe transformation report.
    pub report: PdfAnonymizationReport,
}

/// Reusable PDF anonymizer around a local privacy detector.
pub struct PdfAnonymizer<D> {
    text_anonymizer: ReportAnonymizer<D>,
    options: PdfOptions,
}

impl<D> PdfAnonymizer<D>
where
    D: PrivacyDetector,
{
    /// Construct a PDF anonymizer.
    #[must_use]
    pub fn new(detector: D, report_options: ReportOptions, options: PdfOptions) -> Self {
        Self {
            text_anonymizer: ReportAnonymizer::new(detector, report_options),
            options,
        }
    }

    /// Anonymize a PDF without DICOM-derived associations.
    pub fn anonymize_pdf(&self, input: &[u8]) -> Result<AnonymizedPdf> {
        self.anonymize_pdf_internal(input, None)
    }

    /// Anonymize a PDF using DICOM-derived identifiers as protected linkage spans.
    pub fn anonymize_pdf_with_linkage(
        &self,
        input: &[u8],
        linkage: &SensitiveLinkageMap,
    ) -> Result<AnonymizedPdf> {
        self.anonymize_pdf_internal(input, Some(linkage))
    }

    fn anonymize_pdf_internal(
        &self,
        input: &[u8],
        linkage: Option<&SensitiveLinkageMap>,
    ) -> Result<AnonymizedPdf> {
        if self.options.dpi == 0 {
            return Err(Error::Pdf(
                "rendering DPI must be greater than zero".to_owned(),
            ));
        }
        let _operation = PDFIUM_OPERATION
            .lock()
            .map_err(|_| Error::Pdf("PDFium operation lock is poisoned".to_owned()))?;
        let pdfium = load_pdfium(&self.options.pdfium_library)?;
        let source = pdfium
            .load_pdf_from_byte_slice(input, None)
            .map_err(|error| Error::Pdf(format!("cannot open input: {error}")))?;
        let page_count = u32::try_from(source.pages().len())
            .map_err(|_| Error::Pdf("input returned an invalid page count".to_owned()))?;
        if page_count == 0 {
            return Err(Error::Pdf("input has no pages".to_owned()));
        }
        if page_count > self.options.max_pages {
            return Err(Error::Pdf(format!(
                "input has {page_count} pages, exceeding the configured {}-page limit",
                self.options.max_pages
            )));
        }

        let mut output = pdfium
            .create_new_pdf()
            .map_err(|error| Error::Pdf(format!("cannot create sanitized output: {error}")))?;
        let mut aggregate: Option<ReportAnonymizationReport> = None;
        let mut embedded_text_pages = 0_u32;
        let mut ocr_pages = 0_u32;
        let mut total_render_pixels = 0_u64;

        for index in 0..source.pages().len() {
            let page = source
                .pages()
                .get(index)
                .map_err(|error| Error::Pdf(format!("cannot open page {}: {error}", index + 1)))?;
            let page_width = page.width();
            let page_height = page.height();
            let (pixel_width, pixel_height) = render_dimensions(
                page_width.value,
                page_height.value,
                self.options.dpi,
                self.options.max_render_pixels_per_page,
            )?;
            total_render_pixels = accumulate_rendered_pixels(
                total_render_pixels,
                pixel_width,
                pixel_height,
                self.options.max_total_render_pixels,
            )?;
            let rendered = page
                .render_with_config(
                    &PdfRenderConfig::new().set_fixed_size(pixel_width, pixel_height),
                )
                .map_err(|error| Error::Pdf(format!("cannot render page {}: {error}", index + 1)))?
                .as_image()
                .map_err(|error| {
                    Error::Pdf(format!(
                        "cannot decode rendered page {}: {error}",
                        index + 1
                    ))
                })?;

            let embedded = extract_embedded_text(
                &page,
                rendered.width() as f32 / page_width.value,
                rendered.height() as f32 / page_height.value,
                page_height.value,
            )?;
            let mut located_inputs = Vec::with_capacity(2);
            match self.options.text_mode {
                PdfTextMode::Embedded => {
                    if !has_usable_embedded_text(&embedded.text) {
                        return Err(Error::Pdf(format!(
                            "page {} has no usable embedded text; use --pdf-text-mode auto or ocr",
                            index + 1
                        )));
                    }
                    located_inputs.push(embedded);
                    embedded_text_pages += 1;
                }
                PdfTextMode::Auto => {
                    if has_usable_embedded_text(&embedded.text) {
                        located_inputs.push(embedded);
                        embedded_text_pages += 1;
                    }
                    located_inputs.push(ocr_text(&rendered, &self.options.ocr_language)?);
                    ocr_pages += 1;
                }
                PdfTextMode::Ocr => {
                    located_inputs.push(ocr_text(&rendered, &self.options.ocr_language)?);
                    ocr_pages += 1;
                }
            }

            let mut redactions = Vec::new();
            for located in located_inputs {
                let plan = self.text_anonymizer.plan_text(&located.text, linkage)?;
                let page_report = self.text_anonymizer.report_for_plan(&plan);
                merge_report(&mut aggregate, page_report);
                redactions.extend(locate_redactions(
                    &located,
                    &plan,
                    rendered.width(),
                    rendered.height(),
                )?);
            }
            redactions.sort_by_key(|bounds| (bounds.top, bounds.left, bounds.bottom, bounds.right));
            redactions.dedup();
            let redacted = paint_redactions(rendered, &redactions);

            let mut output_page = output
                .pages_mut()
                .create_page_at_end(PdfPagePaperSize::from_points(page_width, page_height))
                .map_err(|error| {
                    Error::Pdf(format!("cannot create output page {}: {error}", index + 1))
                })?;
            output_page
                .objects_mut()
                .create_image_object(
                    PdfPoints::ZERO,
                    PdfPoints::ZERO,
                    &redacted,
                    Some(page_width),
                    Some(page_height),
                )
                .map_err(|error| {
                    Error::Pdf(format!("cannot write output page {}: {error}", index + 1))
                })?;
        }

        let bytes = output
            .save_to_bytes()
            .map_err(|error| Error::Pdf(format!("cannot serialize sanitized output: {error}")))?;
        Ok(AnonymizedPdf {
            bytes,
            report: PdfAnonymizationReport {
                schema_version: PDF_REPORT_SCHEMA_VERSION,
                privacy: aggregate.expect("a non-empty PDF produces an aggregate report"),
                pages: page_count,
                embedded_text_pages,
                ocr_pages,
                image_only_output: true,
            },
        })
    }
}

#[derive(Debug, Clone)]
struct LocatedText {
    text: String,
    ranges: Vec<LocatedRange>,
}

#[derive(Debug, Clone)]
struct LocatedRange {
    start: usize,
    end: usize,
    bounds: PixelRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn load_pdfium(path: &Path) -> Result<&'static Pdfium> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    if let Some(loaded) = PDFIUM.get() {
        if PDFIUM_PATH
            .get()
            .is_some_and(|loaded_path| loaded_path != &path)
        {
            return Err(Error::Pdf(
                "PDFium was already initialized from a different library path".to_owned(),
            ));
        }
        return Ok(loaded);
    }
    let _guard = PDFIUM_INIT
        .lock()
        .map_err(|_| Error::Pdf("PDFium initialization lock was poisoned".to_owned()))?;
    if PDFIUM.get().is_none() {
        let bindings = Pdfium::bind_to_library(&path).map_err(|error| {
            Error::Pdf(format!("cannot load PDFium {}: {error}", path.display()))
        })?;
        PDFIUM_PATH
            .set(path)
            .map_err(|_| Error::Pdf("PDFium path initialization raced".to_owned()))?;
        PDFIUM
            .set(Pdfium::new(bindings))
            .map_err(|_| Error::Pdf("PDFium initialization raced".to_owned()))?;
    }
    Ok(PDFIUM.get().expect("PDFium was initialized"))
}

fn render_dimensions(
    width_points: f32,
    height_points: f32,
    dpi: u16,
    pixel_limit: u64,
) -> Result<(i32, i32)> {
    let scale = f32::from(dpi) / PDF_POINTS_PER_INCH;
    let width = (width_points * scale).ceil();
    let height = (height_points * scale).ceil();
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return Err(Error::Pdf("page has invalid dimensions".to_owned()));
    }
    let width = width as u64;
    let height = height as u64;
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| Error::Pdf("rendered page dimensions overflow".to_owned()))?;
    if pixels > pixel_limit {
        return Err(Error::Pdf(format!(
            "rendered page has {pixels} pixels, exceeding the configured {pixel_limit}-pixel limit"
        )));
    }
    let width = i32::try_from(width)
        .map_err(|_| Error::Pdf("rendered page width is too large".to_owned()))?;
    let height = i32::try_from(height)
        .map_err(|_| Error::Pdf("rendered page height is too large".to_owned()))?;
    Ok((width, height))
}

fn accumulate_rendered_pixels(current: u64, width: i32, height: i32, limit: u64) -> Result<u64> {
    let width = u64::try_from(width)
        .map_err(|_| Error::Pdf("rendered page width is invalid".to_owned()))?;
    let height = u64::try_from(height)
        .map_err(|_| Error::Pdf("rendered page height is invalid".to_owned()))?;
    let page_pixels = width
        .checked_mul(height)
        .ok_or_else(|| Error::Pdf("rendered page dimensions overflow".to_owned()))?;
    let total = current
        .checked_add(page_pixels)
        .ok_or_else(|| Error::Pdf("total rendered pixel count overflowed".to_owned()))?;
    if total > limit {
        return Err(Error::Pdf(format!(
            "document rendering requires {total} pixels, exceeding the configured {limit}-pixel total limit"
        )));
    }
    Ok(total)
}

fn extract_embedded_text(
    page: &PdfPage<'_>,
    scale_x: f32,
    scale_y: f32,
    page_height: f32,
) -> Result<LocatedText> {
    let page_text = page
        .text()
        .map_err(|error| Error::Pdf(format!("cannot extract embedded text: {error}")))?;
    let mut text = String::new();
    let mut ranges = Vec::new();
    for character in page_text.chars().iter() {
        let Some(value) = character.unicode_char().filter(|value| *value != '\0') else {
            continue;
        };
        let start = text.len();
        text.push(value);
        let end = text.len();
        if value.is_whitespace() {
            continue;
        }
        if let Ok(bounds) = character.tight_bounds() {
            ranges.push(LocatedRange {
                start,
                end,
                bounds: PixelRect {
                    left: (bounds.left().value * scale_x).floor() as i32,
                    top: ((page_height - bounds.top().value) * scale_y).floor() as i32,
                    right: (bounds.right().value * scale_x).ceil() as i32,
                    bottom: ((page_height - bounds.bottom().value) * scale_y).ceil() as i32,
                },
            });
        }
    }
    Ok(LocatedText { text, ranges })
}

fn has_usable_embedded_text(text: &str) -> bool {
    text.chars().filter(|value| !value.is_whitespace()).count()
        >= MIN_EMBEDDED_NON_WHITESPACE_CHARACTERS
}

fn ocr_text(image: &DynamicImage, language: &str) -> Result<LocatedText> {
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|error| Error::Ocr(format!("cannot encode rendered page: {error}")))?;
    let mut tesseract = LepTess::new(None, language)
        .map_err(|error| Error::Ocr(format!("cannot initialize language {language}: {error}")))?;
    tesseract
        .set_image_from_mem(encoded.get_ref())
        .map_err(|error| Error::Ocr(format!("cannot load rendered page: {error}")))?;
    let tsv = tesseract
        .get_tsv_text(0)
        .map_err(|error| Error::Ocr(format!("cannot decode OCR output: {error}")))?;
    parse_tesseract_tsv(&tsv)
}

fn parse_tesseract_tsv(tsv: &str) -> Result<LocatedText> {
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut previous_line: Option<(u32, u32, u32, u32)> = None;
    for row in tsv.lines().skip(1) {
        let columns: Vec<_> = row.splitn(12, '\t').collect();
        if columns.len() != 12 || columns[0] != "5" || columns[11].trim().is_empty() {
            continue;
        }
        let line = (
            parse_tsv_u32(columns[1])?,
            parse_tsv_u32(columns[2])?,
            parse_tsv_u32(columns[3])?,
            parse_tsv_u32(columns[4])?,
        );
        if !text.is_empty() {
            text.push(if previous_line == Some(line) {
                ' '
            } else {
                '\n'
            });
        }
        let word = columns[11].trim();
        let start = text.len();
        text.push_str(word);
        let end = text.len();
        let left = parse_tsv_i32(columns[6])?;
        let top = parse_tsv_i32(columns[7])?;
        let width = parse_tsv_i32(columns[8])?;
        let height = parse_tsv_i32(columns[9])?;
        if width > 0 && height > 0 {
            ranges.push(LocatedRange {
                start,
                end,
                bounds: PixelRect {
                    left,
                    top,
                    right: left.saturating_add(width),
                    bottom: top.saturating_add(height),
                },
            });
        }
        previous_line = Some(line);
    }
    Ok(LocatedText { text, ranges })
}

fn parse_tsv_u32(value: &str) -> Result<u32> {
    value
        .parse()
        .map_err(|_| Error::Ocr("Tesseract returned malformed TSV coordinates".to_owned()))
}

fn parse_tsv_i32(value: &str) -> Result<i32> {
    value
        .parse()
        .map_err(|_| Error::Ocr("Tesseract returned malformed TSV coordinates".to_owned()))
}

fn locate_redactions(
    located: &LocatedText,
    plan: &RedactionPlan,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<PixelRect>> {
    let ranges = plan
        .detected
        .iter()
        .map(|span| (span.start, span.end))
        .chain(plan.linked.iter().map(|span| (span.start, span.end)));
    let mut result = Vec::new();
    for (start, end) in ranges {
        let Some(span) = located.text.get(start..end) else {
            return Err(Error::Pdf(
                "a privacy span could not be mapped to visible page coordinates".to_owned(),
            ));
        };
        for (offset, character) in span.char_indices() {
            if character.is_whitespace() {
                continue;
            }
            let character_start = start + offset;
            let character_end = character_start + character.len_utf8();
            if !located.ranges.iter().any(|range| {
                range.start <= character_start
                    && range.end >= character_end
                    && paintable_bounds(range.bounds, image_width, image_height).is_some()
            }) {
                return Err(Error::Pdf(
                    "a privacy span could not be mapped completely to visible page coordinates"
                        .to_owned(),
                ));
            }
        }
        result.extend(
            located
                .ranges
                .iter()
                .filter(|range| range.start < end && range.end > start)
                .filter(|range| paintable_bounds(range.bounds, image_width, image_height).is_some())
                .map(|range| range.bounds),
        );
    }
    result.sort_by_key(|bounds| (bounds.top, bounds.left, bounds.bottom, bounds.right));
    result.dedup();
    Ok(result)
}

fn paintable_bounds(bounds: PixelRect, image_width: u32, image_height: u32) -> Option<PixelRect> {
    let width = i32::try_from(image_width).unwrap_or(i32::MAX);
    let height = i32::try_from(image_height).unwrap_or(i32::MAX);
    let bounds = PixelRect {
        left: bounds
            .left
            .saturating_sub(REDACTION_PADDING_PIXELS)
            .clamp(0, width),
        top: bounds
            .top
            .saturating_sub(REDACTION_PADDING_PIXELS)
            .clamp(0, height),
        right: bounds
            .right
            .saturating_add(REDACTION_PADDING_PIXELS)
            .clamp(0, width),
        bottom: bounds
            .bottom
            .saturating_add(REDACTION_PADDING_PIXELS)
            .clamp(0, height),
    };
    (bounds.left < bounds.right && bounds.top < bounds.bottom).then_some(bounds)
}

fn paint_redactions(image: DynamicImage, redactions: &[PixelRect]) -> DynamicImage {
    let mut pixels = image.to_rgba8();
    for bounds in redactions {
        let Some(bounds) = paintable_bounds(*bounds, pixels.width(), pixels.height()) else {
            continue;
        };
        for y in bounds.top..bounds.bottom {
            for x in bounds.left..bounds.right {
                pixels.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 255]));
            }
        }
    }
    DynamicImage::ImageRgba8(pixels)
}

fn merge_report(
    aggregate: &mut Option<ReportAnonymizationReport>,
    page: ReportAnonymizationReport,
) {
    let Some(aggregate) = aggregate else {
        *aggregate = Some(page);
        return;
    };
    aggregate.detected_spans += page.detected_spans;
    aggregate.linked_spans += page.linked_spans;
    for (category, count) in page.category_counts {
        *aggregate.category_counts.entry(category).or_insert(0) += count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, DetectedSpan};

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
    fn tesseract_words_are_reconstructed_with_stable_byte_ranges() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t95\tAlice\n\
5\t1\t1\t1\t1\t2\t45\t20\t32\t12\t94\tSmith\n\
5\t1\t1\t1\t2\t1\t10\t40\t20\t12\t90\tOK\n";
        let located = parse_tesseract_tsv(tsv).unwrap();

        assert_eq!(located.text, "Alice Smith\nOK");
        assert_eq!(located.ranges[0].start, 0);
        assert_eq!(located.ranges[1].start, 6);
        assert_eq!(located.ranges[2].start, 12);
        assert_eq!(located.ranges[1].bounds.right, 77);
    }

    #[test]
    fn render_dimensions_enforce_pixel_limit() {
        assert_eq!(
            render_dimensions(612.0, 792.0, 72, 1_000_000).unwrap(),
            (612, 792)
        );
        assert!(render_dimensions(612.0, 792.0, 300, 1_000).is_err());
        assert!(render_dimensions(0.0, 792.0, 72, 1_000_000).is_err());
        assert!(render_dimensions(f32::NAN, 792.0, 72, 1_000_000).is_err());
    }

    #[test]
    fn cumulative_rendered_pixels_are_limited_across_pages() {
        let total = accumulate_rendered_pixels(0, 600, 800, 1_000_000).unwrap();

        assert_eq!(total, 480_000);
        assert!(accumulate_rendered_pixels(total, 600, 800, 900_000).is_err());
        assert!(accumulate_rendered_pixels(0, -1, 800, 900_000).is_err());
        assert!(accumulate_rendered_pixels(u64::MAX, 1, 1, u64::MAX).is_err());
    }

    #[test]
    fn privacy_span_requires_coordinates_for_every_non_whitespace_character() {
        let located = LocatedText {
            text: "AB".to_owned(),
            ranges: vec![LocatedRange {
                start: 0,
                end: 1,
                bounds: PixelRect {
                    left: 10,
                    top: 10,
                    right: 20,
                    bottom: 20,
                },
            }],
        };
        let plan = RedactionPlan {
            detected: vec![DetectedSpan::new(Category::PrivatePerson, 0, 2, 0.99).unwrap()],
            linked: Vec::new(),
        };

        assert!(locate_redactions(&located, &plan, 100, 100).is_err());
    }

    #[test]
    fn privacy_span_accepts_unlocated_whitespace_between_visible_characters() {
        let located = LocatedText {
            text: "A B".to_owned(),
            ranges: vec![
                LocatedRange {
                    start: 0,
                    end: 1,
                    bounds: PixelRect {
                        left: 10,
                        top: 10,
                        right: 20,
                        bottom: 20,
                    },
                },
                LocatedRange {
                    start: 2,
                    end: 3,
                    bounds: PixelRect {
                        left: 30,
                        top: 10,
                        right: 40,
                        bottom: 20,
                    },
                },
            ],
        };
        let plan = RedactionPlan {
            detected: vec![DetectedSpan::new(Category::PrivatePerson, 0, 3, 0.99).unwrap()],
            linked: Vec::new(),
        };

        assert_eq!(
            locate_redactions(&located, &plan, 100, 100).unwrap().len(),
            2
        );
    }

    #[test]
    fn privacy_span_rejects_coordinates_outside_the_rendered_page() {
        let located = LocatedText {
            text: "A".to_owned(),
            ranges: vec![LocatedRange {
                start: 0,
                end: 1,
                bounds: PixelRect {
                    left: 200,
                    top: 200,
                    right: 210,
                    bottom: 210,
                },
            }],
        };
        let plan = RedactionPlan {
            detected: vec![DetectedSpan::new(Category::PrivatePerson, 0, 1, 0.99).unwrap()],
            linked: Vec::new(),
        };

        assert!(locate_redactions(&located, &plan, 100, 100).is_err());
    }

    #[test]
    fn malformed_tesseract_coordinates_fail_without_partial_locations() {
        let invalid = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\tinvalid\t1\t1\t1\t1\t10\t20\t30\t12\t95\tAlice\n";
        assert!(parse_tesseract_tsv(invalid).is_err());

        let zero_size = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t20\t0\t12\t95\tAlice\n";
        let located = parse_tesseract_tsv(zero_size).unwrap();
        assert_eq!(located.text, "Alice");
        assert!(located.ranges.is_empty());
    }

    #[test]
    fn embedded_text_usability_requires_enough_visible_characters() {
        assert!(!has_usable_embedded_text("a b c d"));
        assert!(has_usable_embedded_text("a b c d e f g h"));
    }

    #[test]
    fn page_reports_merge_counts_and_categories() {
        let mut aggregate = None;
        let first = ReportAnonymizationReport {
            schema_version: 1,
            detected_spans: 1,
            linked_spans: 0,
            category_counts: [(Category::PrivatePerson, 1)].into_iter().collect(),
            model: None,
            human_review_required: true,
        };
        let second = ReportAnonymizationReport {
            schema_version: 1,
            detected_spans: 1,
            linked_spans: 2,
            category_counts: [(Category::PrivatePerson, 1), (Category::Secret, 1)]
                .into_iter()
                .collect(),
            model: None,
            human_review_required: true,
        };

        merge_report(&mut aggregate, first);
        merge_report(&mut aggregate, second);
        let aggregate = aggregate.unwrap();

        assert_eq!(aggregate.detected_spans, 2);
        assert_eq!(aggregate.linked_spans, 2);
        assert_eq!(aggregate.category_counts[&Category::PrivatePerson], 2);
        assert_eq!(aggregate.category_counts[&Category::Secret], 1);
    }

    #[test]
    fn redaction_rectangles_are_painted_into_page_pixels() {
        let image = DynamicImage::new_rgba8(20, 20);
        let painted = paint_redactions(
            image,
            &[PixelRect {
                left: 7,
                top: 7,
                right: 11,
                bottom: 11,
            }],
        )
        .to_rgba8();

        assert_eq!(painted.get_pixel(8, 8), &Rgba([0, 0, 0, 255]));
        assert_eq!(painted.get_pixel(3, 3), &Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn detected_text_is_mapped_and_painted_on_the_rendered_page() {
        const CHARACTER_WIDTH: i32 = 6;
        const GLYPH_TOP: i32 = 5;
        const GLYPH_BOTTOM: i32 = 15;
        const IMAGE_WIDTH: u32 = 200;
        const IMAGE_HEIGHT: u32 = 20;

        let text = "Patient Alice Smith is stable";
        let ranges = text
            .char_indices()
            .enumerate()
            .filter(|(_, (_, character))| !character.is_whitespace())
            .map(|(index, (start, character))| {
                let left = i32::try_from(index).unwrap() * CHARACTER_WIDTH;
                LocatedRange {
                    start,
                    end: start + character.len_utf8(),
                    bounds: PixelRect {
                        left,
                        top: GLYPH_TOP,
                        right: left + CHARACTER_WIDTH,
                        bottom: GLYPH_BOTTOM,
                    },
                }
            })
            .collect::<Vec<_>>();
        let located = LocatedText {
            text: text.to_owned(),
            ranges,
        };
        let anonymizer = ReportAnonymizer::new(NameDetector, ReportOptions::default());
        let plan = anonymizer.plan_text(text, None).unwrap();
        let redactions = locate_redactions(&located, &plan, IMAGE_WIDTH, IMAGE_HEIGHT).unwrap();
        let painted = paint_redactions(
            DynamicImage::new_rgba8(IMAGE_WIDTH, IMAGE_HEIGHT),
            &redactions,
        )
        .to_rgba8();

        let name_start = text.find("Alice Smith").unwrap();
        let name_end = name_start + "Alice Smith".len();
        for range in located
            .ranges
            .iter()
            .filter(|range| range.start < name_end && range.end > name_start)
        {
            let center_x = (range.bounds.left + range.bounds.right) / 2;
            assert_eq!(
                painted.get_pixel(center_x as u32, 10),
                &Rgba([0, 0, 0, 255])
            );
        }
        assert_eq!(painted.get_pixel(3, 10), &Rgba([0, 0, 0, 0]));
    }

    #[test]
    #[ignore = "requires a dynamically loadable PDFium library"]
    fn sanitized_pdf_renders_black_pixels_over_detected_text() {
        let pdfium_library =
            PathBuf::from(std::env::var("DICOM_ANON_REPORT_PDFIUM_LIBRARY").unwrap());
        let input = minimal_text_pdf(
            b"% leading transport comment\n",
            "Patient Alice Smith has a normal exam.",
        );
        let dpi = 200;
        let expected = source_name_rectangles(&pdfium_library, &input, dpi);
        let anonymizer = PdfAnonymizer::new(
            NameDetector,
            ReportOptions::default(),
            PdfOptions {
                pdfium_library: pdfium_library.clone(),
                dpi,
                text_mode: PdfTextMode::Embedded,
                ..PdfOptions::default()
            },
        );

        let output = anonymizer.anonymize_pdf(&input).unwrap();
        let rendered = render_first_page(&pdfium_library, &output.bytes, dpi);
        let pixels = rendered.to_rgba8();

        assert!(!expected.is_empty());
        for bounds in expected {
            let x = ((bounds.left + bounds.right) / 2).clamp(0, pixels.width() as i32 - 1);
            let y = ((bounds.top + bounds.bottom) / 2).clamp(0, pixels.height() as i32 - 1);
            let pixel = pixels.get_pixel(x as u32, y as u32);
            assert!(
                pixel.0[..3].iter().all(|channel| *channel <= 8),
                "expected dark redaction pixel at ({x}, {y}), got {pixel:?}"
            );
        }
    }

    fn source_name_rectangles(path: &Path, input: &[u8], dpi: u16) -> Vec<PixelRect> {
        let _operation = PDFIUM_OPERATION.lock().unwrap();
        let pdfium = load_pdfium(path).unwrap();
        let document = pdfium.load_pdf_from_byte_slice(input, None).unwrap();
        let page = document.pages().get(0).unwrap();
        let (width, height) = render_dimensions(
            page.width().value,
            page.height().value,
            dpi,
            DEFAULT_MAX_RENDER_PIXELS,
        )
        .unwrap();
        let rendered = page
            .render_with_config(&PdfRenderConfig::new().set_fixed_size(width, height))
            .unwrap()
            .as_image()
            .unwrap();
        let located = extract_embedded_text(
            &page,
            rendered.width() as f32 / page.width().value,
            rendered.height() as f32 / page.height().value,
            page.height().value,
        )
        .unwrap();
        let start = located.text.find("Alice Smith").unwrap();
        let end = start + "Alice Smith".len();
        located
            .ranges
            .into_iter()
            .filter(|range| range.start < end && range.end > start)
            .map(|range| range.bounds)
            .collect()
    }

    fn render_first_page(path: &Path, input: &[u8], dpi: u16) -> DynamicImage {
        let _operation = PDFIUM_OPERATION.lock().unwrap();
        let pdfium = load_pdfium(path).unwrap();
        let document = pdfium.load_pdf_from_byte_slice(input, None).unwrap();
        let page = document.pages().get(0).unwrap();
        let (width, height) = render_dimensions(
            page.width().value,
            page.height().value,
            dpi,
            DEFAULT_MAX_RENDER_PIXELS,
        )
        .unwrap();
        let rendered = page
            .render_with_config(&PdfRenderConfig::new().set_fixed_size(width, height))
            .unwrap()
            .as_image()
            .unwrap();
        rendered
    }

    fn minimal_text_pdf(prefix: &[u8], text: &str) -> Vec<u8> {
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
        let mut pdf = prefix.to_vec();
        pdf.extend_from_slice(b"%PDF-1.4\n");
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
}
