//! Local report anonymization with optional DICOM-aware linkage.

mod decoder;
mod error;
mod install;
mod linkage;
mod model;
#[cfg(feature = "pdf")]
mod pdf;
mod redact;

pub use decoder::ViterbiDecoder;
pub use error::{Error, Result};
pub use install::{
    install_assets, AssetBackend, AssetInstallOptions, InstalledAssetManifest,
    ONNX_RUNTIME_VERSION, PDFIUM_REVISION, PRIVACY_FILTER_REPOSITORY, PRIVACY_FILTER_REVISION,
};
pub use linkage::{AssociationManifest, DicomAssociation, DicomLinkage, SensitiveLinkageMap};
pub use model::{Device, ModelBundle, PrivacyFilter, DEFAULT_MAX_MODEL_TOKENS};
#[cfg(feature = "pdf")]
pub use pdf::{AnonymizedPdf, PdfAnonymizationReport, PdfAnonymizer, PdfOptions, PdfTextMode};
pub use redact::{
    AnonymizedDocument, Category, DetectedSpan, DetectorMetadata, PrivacyDetector,
    ReportAnonymizationReport, ReportAnonymizer, ReportOptions,
};
