//! FastPDF engine adapter for hayro (skeleton; implementation in progress).

use fastpdf_engine_api::{
    DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo, OpenOptions,
    PdfEngine,
};

/// The hayro engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct HayroEngine;

impl HayroEngine {
    pub fn new() -> Self {
        Self
    }
}

impl PdfEngine for HayroEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "hayro",
            version: "unimplemented",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        _source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Err(EngineError::Unsupported("hayro adapter not implemented yet".into()))
    }
}
