//! Compile-time engine registry (spec §43: `--features engine-hayro`,
//! `--features engine-zpdf`) with runtime selection (`--engine NAME`).

use fastpdf_engine_api::PdfEngine;

/// Every engine compiled into this binary, in preference order.
// The pushes are cfg-dependent, so `vec![...]` cannot replace them.
#[allow(clippy::vec_init_then_push)]
pub(crate) fn all() -> Vec<Box<dyn PdfEngine>> {
    #[allow(unused_mut)]
    let mut engines: Vec<Box<dyn PdfEngine>> = Vec::new();
    #[cfg(feature = "engine-hayro")]
    engines.push(Box::new(fastpdf_engine_hayro::HayroEngine::new()));
    #[cfg(feature = "engine-zpdf")]
    engines.push(Box::new(fastpdf_engine_zpdf::ZpdfEngine::new()));
    engines
}

type EnginePair = (Box<dyn PdfEngine>, Box<dyn PdfEngine>);

/// Two engines for `diff`: `spec` is `"a,b"`; without it, the first two
/// compiled-in engines.
pub(crate) fn select_pair(spec: Option<&str>) -> Result<EnginePair, String> {
    match spec.and_then(|s| s.split_once(',')) {
        Some((a, b)) => Ok((select(Some(a.trim()))?, select(Some(b.trim()))?)),
        None => {
            let mut engines = all().into_iter();
            match (engines.next(), engines.next()) {
                (Some(a), Some(b)) => Ok((a, b)),
                _ => Err("diff needs two engines; build with --features engine-zpdf".into()),
            }
        }
    }
}

/// The engine named `name`, or the first compiled-in engine.
pub(crate) fn select(name: Option<&str>) -> Result<Box<dyn PdfEngine>, String> {
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
                        "engine `{name}` not compiled in (available: {})",
                        names.join(", ")
                    )
                })?;
            Ok(engines.remove(index))
        }
    }
}
