//! Compile-time engine registry (spec §43: `--features engine-hayro`,
//! `--features engine-zpdf`) with runtime selection (`--engine NAME` or
//! `FASTPDF_ENGINE=NAME`). Same rules as `fastpdf-bench`. `NAME-isolated`
//! selects the same engine in a render host process (opt-in, ADR 0008;
//! `render_host.rs`).

use fastpdf_engine_api::PdfEngine;

use crate::render_host::{self, ISOLATED_SUFFIX};

/// `hayro-isolated` -> `hayro` (case-insensitive suffix).
fn strip_isolated(name: &str) -> Option<&str> {
    let cut = name.len().checked_sub(ISOLATED_SUFFIX.len())?;
    (name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(ISOLATED_SUFFIX))
        .then(|| &name[..cut])
        .filter(|base| !base.is_empty())
}

/// Every engine compiled into this binary, in preference order.
// The pushes are cfg-dependent, so `vec![...]` cannot replace them.
#[allow(clippy::vec_init_then_push)]
fn all() -> Vec<Box<dyn PdfEngine>> {
    #[allow(unused_mut)]
    let mut engines: Vec<Box<dyn PdfEngine>> = Vec::new();
    #[cfg(feature = "engine-hayro")]
    engines.push(Box::new(fastpdf_engine_hayro::HayroEngine::new()));
    #[cfg(feature = "engine-zpdf")]
    engines.push(Box::new(fastpdf_engine_zpdf::ZpdfEngine::new()));
    #[cfg(feature = "engine-synthetic")]
    engines.push(Box::new(crate::synthetic::SyntheticEngine));
    engines
}

/// Names of the compiled-in engines, for `--help`.
pub(crate) fn names() -> Vec<String> {
    let base: Vec<&'static str> = all().iter().map(|e| e.info().name).collect();
    let isolated = base.iter().map(|n| format!("{n}{ISOLATED_SUFFIX}"));
    base.iter()
        .map(|n| (*n).to_owned())
        .chain(isolated)
        .collect()
}

/// The in-process engine named exactly `name`; the render host's factory.
pub(crate) fn create(name: &str) -> Option<Box<dyn PdfEngine>> {
    all().into_iter().find(|e| e.info().name == name)
}

/// The engine named `name`, or the first compiled-in engine.
pub(crate) fn select(name: Option<&str>) -> Result<Box<dyn PdfEngine>, String> {
    if let Some(base) = name.and_then(|n| strip_isolated(n)) {
        return select(Some(base)).map(render_host::isolate);
    }
    let mut engines = all();
    if engines.is_empty() {
        return Err(
            "no engine compiled in; build with --features engine-hayro and/or engine-zpdf".into(),
        );
    }
    match name {
        None => Ok(engines.remove(0)),
        Some(name) => {
            let names: Vec<&'static str> = engines.iter().map(|e| e.info().name).collect();
            let index = names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    format!(
                        "engine `{name}` is not compiled in (available: {})",
                        names.join(", ")
                    )
                })?;
            Ok(engines.remove(index))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_names_strip_the_suffix() {
        assert_eq!(strip_isolated("hayro-isolated"), Some("hayro"));
        assert_eq!(strip_isolated("ZPDF-Isolated"), Some("ZPDF"));
        assert_eq!(strip_isolated("-isolated"), None);
        assert_eq!(strip_isolated("hayro"), None);
        assert_eq!(strip_isolated("測"), None);
    }

    #[test]
    fn every_engine_has_an_isolated_name() {
        let names = names();
        for engine in all() {
            let isolated = format!("{}{ISOLATED_SUFFIX}", engine.info().name);
            assert!(names.contains(&isolated), "{names:?}");
            assert!(create(engine.info().name).is_some());
        }
        assert!(create("no-such-engine").is_none());
    }
}
