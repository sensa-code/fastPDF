//! FastPDF engine adapter for zpdf (skeleton; implementation in progress).

use fastpdf_engine_api::{
    DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo, OpenOptions,
    PdfEngine,
};

/// The zpdf engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZpdfEngine;

impl ZpdfEngine {
    pub fn new() -> Self {
        Self
    }
}

impl PdfEngine for ZpdfEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "zpdf",
            version: "unimplemented",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        _source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Err(EngineError::Unsupported("zpdf adapter not implemented yet".into()))
    }
}
