use super::*;
use crate::{
    CancelToken, DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo,
    OpenOptions, PageIndex, PageInfo, PageSize, PdfEngine, PixmapMut, RenderOutcome, RenderRequest,
    Rotation, SharedBytes, open_guarded,
};

#[test]
fn permission_bits_read_like_the_spec() {
    // -4: every bit but 1 and 2.
    let all = Permissions::from_p(-4, 3);
    assert!(all.all_allowed());
    assert_eq!(all.printing(), PrintPermission::HighResolution);

    // -3904: bits 7, 8 and 13-32 only; nothing is allowed.
    let none = Permissions::from_p(-3904, 3);
    assert_eq!(none.printing(), PrintPermission::NotAllowed);
    assert!(!none.modify() && !none.copy() && !none.annotate());
    assert!(!none.fill_forms() && !none.assemble() && !none.copy_for_accessibility());
    assert!(!none.all_allowed());
    // Written unsigned, it is the same number.
    assert_eq!(Permissions::from_p(4_294_963_392, 3), none);
    assert_eq!(none.bits, 4_294_963_392);

    // Print only (pypdf: bits 3 and 12).
    let print_only = Permissions::from_p(0xFFFF_F000 | (1 << 2) | (1 << 11), 4);
    assert_eq!(print_only.printing(), PrintPermission::HighResolution);
    assert!(!print_only.copy() && !print_only.modify());
    assert!(!print_only.all_allowed());
}

#[test]
fn revisions_read_bits_differently() {
    // Bit 3 without bit 12: low resolution from revision 3 on.
    let p = i64::from(0xFFFF_F0C0_u32 | (1 << 2));
    assert_eq!(
        Permissions::from_p(p, 3).printing(),
        PrintPermission::LowResolution
    );
    assert_eq!(
        Permissions::from_p(p, 2).printing(),
        PrintPermission::HighResolution
    );
    // Bits 9 and 11 count from revision 3 on; revision 2 falls back to
    // bits 6 and 4.
    let forms_and_pages = i64::from(0xFFFF_F0C0_u32 | (1 << 8) | (1 << 10));
    let r3 = Permissions::from_p(forms_and_pages, 3);
    assert!(r3.fill_forms() && r3.assemble() && !r3.annotate() && !r3.modify());
    let r2 = Permissions::from_p(forms_and_pages, 2);
    assert!(!r2.fill_forms() && !r2.assemble());
    let r2 = Permissions::from_p(forms_and_pages | (1 << 5) | (1 << 3), 2);
    assert!(r2.fill_forms() && r2.assemble());
    // Copying implies copying for accessibility; bit 10 alone allows only
    // the latter.
    let accessibility = Permissions::from_p(i64::from(0xFFFF_F0C0_u32 | (1 << 9)), 3);
    assert!(accessibility.copy_for_accessibility() && !accessibility.copy());
    // Values outside 32 bits never panic.
    let _ = Permissions::from_p(i64::MIN, 4).printing();
    let _ = Permissions::from_p(i64::MAX, 0).all_allowed();
}

/// An engine whose documents answer metadata, and panic for properties
/// when asked to.
struct Engine {
    panic_in_properties: bool,
}

struct Doc {
    panic_in_properties: bool,
}

fn metadata() -> DocumentMetadata {
    DocumentMetadata {
        title: Some("標題".into()),
        pdf_version: Some("1.7".into()),
        encrypted: true,
        ..DocumentMetadata::default()
    }
}

impl PdfEngine for Engine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "properties-test",
            version: "0",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        _source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Ok(Box::new(Doc {
            panic_in_properties: self.panic_in_properties,
        }))
    }
}

impl EngineDocument for Doc {
    fn page_count(&self) -> u32 {
        1
    }

    fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        Ok(metadata())
    }

    fn properties(&self) -> Result<DocumentProperties, EngineError> {
        if self.panic_in_properties {
            panic!("form tree exploded");
        }
        Ok(DocumentProperties::from_metadata(self.metadata()?))
    }

    fn render(
        &self,
        _request: &RenderRequest,
        _target: &mut PixmapMut<'_>,
        _cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        Ok(RenderOutcome::default())
    }
}

/// Uses the trait's default `properties`.
struct Plain;

impl EngineDocument for Plain {
    fn page_count(&self) -> u32 {
        1
    }

    fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
        Err(EngineError::Unsupported("geometry".into()))
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        Ok(metadata())
    }

    fn render(
        &self,
        _request: &RenderRequest,
        _target: &mut PixmapMut<'_>,
        _cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        Ok(RenderOutcome::default())
    }
}

fn open(panic_in_properties: bool) -> crate::GuardedDocument {
    open_guarded(
        &Engine {
            panic_in_properties,
        },
        DocumentSource::from_bytes(SharedBytes::from_vec(b"%PDF".to_vec())),
        &OpenOptions::default(),
    )
    .unwrap_or_else(|e| panic!("opens: {e}"))
}

#[test]
fn the_default_is_the_metadata_and_nothing_else() {
    let properties = Plain.properties();
    assert_eq!(
        properties,
        Ok(DocumentProperties {
            metadata: metadata(),
            security: None,
            forms: FormSummary::default(),
            tagged: false,
            linearized: false,
        })
    );
    // The guard passes the engine's answer on.
    assert_eq!(
        open(false).properties(),
        Ok(DocumentProperties::from_metadata(metadata()))
    );
}

#[test]
fn panics_reading_properties_are_contained_and_counted() {
    let doc = open(true);
    for _ in 0..2 {
        assert!(matches!(
            doc.properties(),
            Err(EngineError::Panicked(m)) if m.contains("exploded")
        ));
    }
    assert_eq!(doc.panic_count(), 2);
    // The rest of the document keeps working.
    assert_eq!(doc.metadata(), Ok(metadata()));
}
