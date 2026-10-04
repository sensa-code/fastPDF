//! Font resolver for fonts a PDF does not embed (spec §27: Traditional
//! Chinese documents very often use non-embedded `MSung-Light` / `MHei-Medium`).
//!
//! hayro asks for two kinds of fonts:
//! * `FontQuery::Standard`: the 14 standard fonts (and every non-embedded
//!   simple font, which hayro maps onto one of them). Helvetica, Times and
//!   Courier map to Windows' metric-compatible Arial, Times New Roman and
//!   Courier New; Symbol and ZapfDingbats (and everything when Windows fonts
//!   are missing) use hayro's embedded Foxit fonts.
//! * `FontQuery::Fallback`: non-embedded CID fonts. The character collection
//!   (Adobe-CNS1 / GB1 / Japan1 / Korea1) and the font name pick a Windows
//!   CJK font; hayro then maps CID → Unicode → glyph, so any font with the
//!   characters works.
//!
//! Font files are memory-mapped (not read) and shared process-wide through a
//! small bounded cache, so a 28 MB `mingliu.ttc` only costs the pages hayro
//! actually touches.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use hayro::hayro_interpret::FontResolverFn;
use hayro::hayro_interpret::font::{FallbackFontQuery, FontData, FontQuery, StandardFont};
use hayro::hayro_interpret::hayro_cmap::CidFamily;

/// Mapped font files kept open. Mappings cost address space and file-backed
/// pages only, but each one also keeps a file handle, so the count is capped.
const MAX_CACHED_FONT_FILES: usize = 24;
/// Font files larger than this are never mapped or read (hostile font
/// directories, corrupted files). The largest stock Windows CJK font is
/// ~30 MB.
const MAX_FONT_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Counters for diagnostics and tests.
#[derive(Debug, Default)]
pub(crate) struct FontStats {
    pub(crate) system_hits: AtomicU64,
    pub(crate) cjk_hits: AtomicU64,
    pub(crate) embedded_fallbacks: AtomicU64,
    pub(crate) misses: AtomicU64,
}

struct FontStore {
    dirs: Vec<PathBuf>,
    cache: Mutex<LruFiles>,
    stats: FontStats,
}

#[derive(Default)]
struct LruFiles {
    /// File name (lower case) -> mapped font, or `None` when absent.
    entries: HashMap<String, Option<FontData>>,
    order: VecDeque<String>,
}

fn store() -> &'static FontStore {
    static STORE: OnceLock<FontStore> = OnceLock::new();
    STORE.get_or_init(|| FontStore {
        dirs: font_dirs(),
        cache: Mutex::new(LruFiles::default()),
        stats: FontStats::default(),
    })
}

pub(crate) fn stats() -> &'static FontStats {
    &store().stats
}

/// Bytes of the system font files currently mapped by the store. They are
/// file-backed and shared (not private memory) and can outlive the store's
/// LRU while hayro's per-thread caches still use them.
pub(crate) fn mapped_bytes() -> u64 {
    let cache = lock(&store().cache);
    cache
        .entries
        .values()
        .flatten()
        .map(|data| (**data).as_ref().len() as u64)
        .sum()
}

/// The resolver handed to hayro. One shared `Arc`, so cloning settings is free.
pub(crate) fn resolver() -> FontResolverFn {
    static RESOLVER: OnceLock<FontResolverFn> = OnceLock::new();
    RESOLVER
        .get_or_init(|| Arc::new(|query: &FontQuery| resolve(query)))
        .clone()
}

fn resolve(query: &FontQuery) -> Option<(FontData, u32)> {
    let store = store();
    let found = match query {
        FontQuery::Standard(font) => standard(store, *font),
        FontQuery::Fallback(f) => {
            let cjk = cjk_candidates(f);
            if !cjk.is_empty() {
                if let Some(hit) = first_available(store, &cjk) {
                    store.stats.cjk_hits.fetch_add(1, Ordering::Relaxed);
                    return Some(hit);
                }
                store.stats.misses.fetch_add(1, Ordering::Relaxed);
            }
            first_available(store, &named_candidates(f))
                .or_else(|| standard(store, f.pick_standard_font()))
        }
    };
    if found.is_none() {
        store.stats.misses.fetch_add(1, Ordering::Relaxed);
    }
    found
}

fn standard(store: &FontStore, font: StandardFont) -> Option<(FontData, u32)> {
    if let Some(file) = standard_file(font)
        && let Some(hit) = first_available(store, &[Candidate::new(file, 0)])
    {
        return Some(hit);
    }
    store
        .stats
        .embedded_fallbacks
        .fetch_add(1, Ordering::Relaxed);
    Some(font.get_font_data())
}

/// Windows core fonts are metric-compatible with the standard 14 and cover
/// far more glyphs than the embedded Foxit set. Symbol and ZapfDingbats keep
/// the embedded fonts: their glyph names must match Adobe's.
fn standard_file(font: StandardFont) -> Option<&'static str> {
    Some(match font {
        StandardFont::Helvetica => "arial.ttf",
        StandardFont::HelveticaBold => "arialbd.ttf",
        StandardFont::HelveticaOblique => "ariali.ttf",
        StandardFont::HelveticaBoldOblique => "arialbi.ttf",
        StandardFont::TimesRoman => "times.ttf",
        StandardFont::TimesBold => "timesbd.ttf",
        StandardFont::TimesItalic => "timesi.ttf",
        StandardFont::TimesBoldItalic => "timesbi.ttf",
        StandardFont::Courier => "cour.ttf",
        StandardFont::CourierBold => "courbd.ttf",
        StandardFont::CourierOblique => "couri.ttf",
        StandardFont::CourierBoldOblique => "courbi.ttf",
        StandardFont::Symbol | StandardFont::ZapfDingBats => return None,
    })
}

/// A font file and the face to use inside a TrueType collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) file: &'static str,
    pub(crate) face: u32,
}

impl Candidate {
    const fn new(file: &'static str, face: u32) -> Self {
        Self { file, face }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Serif,
    Sans,
    Kai,
}

/// Style hint from the PostScript / font name (`MSung-Light`, `MHei-Medium`,
/// `HeiseiKakuGo-W5`, `HYGoThic-Medium`, `STSong-Light`, `DFKaiShu-SB`, ...).
fn style_of(f: &FallbackFontQuery) -> Style {
    let name = [&f.post_script_name, &f.font_name, &f.font_family]
        .iter()
        .filter_map(|n| n.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if name.contains("kai") {
        Style::Kai
    } else if [
        "hei", "gothic", "goth", "kakugo", "sans", "dotum", "gulim", "maru",
    ]
    .iter()
    .any(|k| name.contains(k))
    {
        Style::Sans
    } else if [
        "sung", "song", "ming", "min", "mincho", "serif", "batang", "myeongjo", "myungjo",
    ]
    .iter()
    .any(|k| name.contains(k))
        || f.is_serif
    {
        Style::Serif
    } else {
        // CJK body text is far more often Ming/Song than Hei.
        Style::Serif
    }
}

/// Windows CJK fonts for a non-embedded CID font, best first.
pub(crate) fn cjk_candidates(f: &FallbackFontQuery) -> Vec<Candidate> {
    let Some(cc) = &f.character_collection else {
        return Vec::new();
    };
    let style = style_of(f);
    let bold = f.is_bold || f.font_weight >= 600;
    let c = Candidate::new;
    match cc.family {
        CidFamily::AdobeCNS1 => match (style, bold) {
            (Style::Kai, _) => vec![c("kaiu.ttf", 0), c("mingliu.ttc", 1), c("msjh.ttc", 0)],
            (Style::Sans, true) => vec![c("msjhbd.ttc", 0), c("msjh.ttc", 0), c("mingliu.ttc", 1)],
            (Style::Sans, false) => vec![c("msjh.ttc", 0), c("mingliu.ttc", 1)],
            // PMingLiU (face 1) is the proportional MingLiU.
            (Style::Serif, _) => vec![c("mingliu.ttc", 1), c("msjh.ttc", 0)],
        },
        CidFamily::AdobeGB1 => match (style, bold) {
            (Style::Kai, _) => vec![c("simkai.ttf", 0), c("simsun.ttc", 0), c("msyh.ttc", 0)],
            (Style::Sans, true) => vec![c("msyhbd.ttc", 0), c("simhei.ttf", 0), c("msyh.ttc", 0)],
            (Style::Sans, false) => vec![c("msyh.ttc", 0), c("simhei.ttf", 0), c("simsun.ttc", 0)],
            (Style::Serif, _) => vec![c("simsun.ttc", 0), c("msyh.ttc", 0)],
        },
        CidFamily::AdobeJapan1 => match (style, bold) {
            (Style::Sans, true) => vec![
                c("YuGothB.ttc", 0),
                c("msgothic.ttc", 0),
                c("YuGothM.ttc", 0),
            ],
            (Style::Sans, false) | (Style::Kai, false) => {
                vec![
                    c("YuGothM.ttc", 0),
                    c("msgothic.ttc", 0),
                    c("meiryo.ttc", 0),
                ]
            }
            // Mincho fonts are optional features on Windows 11; fall back
            // to Gothic so text still renders.
            _ => vec![
                c("yumin.ttf", 0),
                c("msmincho.ttc", 0),
                c("YuGothM.ttc", 0),
                c("msgothic.ttc", 0),
            ],
        },
        CidFamily::AdobeKorea1 => match (style, bold) {
            (Style::Sans, true) => vec![c("malgunbd.ttf", 0), c("malgun.ttf", 0)],
            (Style::Serif, _) => vec![c("batang.ttc", 0), c("malgun.ttf", 0)],
            _ => vec![c("malgun.ttf", 0), c("gulim.ttc", 0)],
        },
        // Identity / custom collections carry no language; use the name.
        CidFamily::AdobeIdentity | CidFamily::Custom { .. } => named_candidates(f),
    }
}

/// Windows fonts for well-known font names that a PDF referenced without
/// embedding them.
fn named_candidates(f: &FallbackFontQuery) -> Vec<Candidate> {
    let name = f
        .post_script_name
        .as_deref()
        .or(f.font_name.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace([' ', '-', '_', ','], "");
    let c = Candidate::new;
    let table: &[(&str, Candidate)] = &[
        ("pmingliu", c("mingliu.ttc", 1)),
        ("mingliu", c("mingliu.ttc", 0)),
        ("dfkai", c("kaiu.ttf", 0)),
        ("kaiu", c("kaiu.ttf", 0)),
        ("microsoftjhenghei", c("msjh.ttc", 0)),
        ("jhenghei", c("msjh.ttc", 0)),
        ("simsun", c("simsun.ttc", 0)),
        ("nsimsun", c("simsun.ttc", 1)),
        ("simhei", c("simhei.ttf", 0)),
        ("microsoftyahei", c("msyh.ttc", 0)),
        ("yahei", c("msyh.ttc", 0)),
        ("msgothic", c("msgothic.ttc", 0)),
        ("mspgothic", c("msgothic.ttc", 2)),
        ("msmincho", c("msmincho.ttc", 0)),
        ("yugothic", c("YuGothM.ttc", 0)),
        ("meiryo", c("meiryo.ttc", 0)),
        ("malgungothic", c("malgun.ttf", 0)),
        ("batang", c("batang.ttc", 0)),
        ("gulim", c("gulim.ttc", 0)),
        ("arial", c("arial.ttf", 0)),
        ("timesnewroman", c("times.ttf", 0)),
        ("couriernew", c("cour.ttf", 0)),
        ("calibri", c("calibri.ttf", 0)),
        ("cambria", c("cambria.ttc", 0)),
        ("georgia", c("georgia.ttf", 0)),
        ("verdana", c("verdana.ttf", 0)),
        ("tahoma", c("tahoma.ttf", 0)),
        ("segoeui", c("segoeui.ttf", 0)),
        ("consolas", c("consola.ttf", 0)),
    ];
    table
        .iter()
        .filter(|(key, _)| name.contains(key))
        .map(|(_, cand)| *cand)
        .collect()
}

fn first_available(store: &FontStore, candidates: &[Candidate]) -> Option<(FontData, u32)> {
    for cand in candidates {
        if let Some(data) = load(store, cand.file) {
            let faces = ttc_face_count(data.as_ref().as_ref());
            let face = if cand.face < faces { cand.face } else { 0 };
            store.stats.system_hits.fetch_add(1, Ordering::Relaxed);
            return Some((data, face));
        }
    }
    None
}

/// Number of faces in a TrueType collection (1 for plain font files).
fn ttc_face_count(data: &[u8]) -> u32 {
    if data.get(..4) == Some(b"ttcf")
        && let Some(n) = data.get(8..12)
    {
        return u32::from_be_bytes([n[0], n[1], n[2], n[3]]).clamp(1, 64);
    }
    1
}

fn load(store: &FontStore, file: &str) -> Option<FontData> {
    let key = file.to_ascii_lowercase();
    {
        let mut cache = lock(&store.cache);
        if let Some(hit) = cache.entries.get(&key).cloned() {
            touch(&mut cache, &key);
            return hit;
        }
    }
    // Map outside the lock; a duplicate mapping on a race is harmless.
    let loaded = store
        .dirs
        .iter()
        .map(|dir| dir.join(file))
        .find_map(|path| open_font(&path));
    let mut cache = lock(&store.cache);
    if !cache.entries.contains_key(&key) {
        cache.entries.insert(key.clone(), loaded.clone());
        cache.order.push_back(key);
        while cache.order.len() > MAX_CACHED_FONT_FILES {
            if let Some(old) = cache.order.pop_front() {
                cache.entries.remove(&old);
            }
        }
    }
    loaded
}

fn touch(cache: &mut LruFiles, key: &str) {
    if let Some(pos) = cache.order.iter().position(|k| k == key)
        && let Some(k) = cache.order.remove(pos)
    {
        cache.order.push_back(k);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR")
            .or_else(|| std::env::var_os("SystemRoot"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }
    dirs
}

/// A read-only mapping of a font file plus the handle that keeps writers out.
struct MappedFont {
    map: memmap2::Mmap,
    _file: File,
}

impl AsRef<[u8]> for MappedFont {
    fn as_ref(&self) -> &[u8] {
        &self.map
    }
}

fn open_font(path: &std::path::Path) -> Option<FontData> {
    let file = open_deny_write(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 || len > MAX_FONT_FILE_BYTES {
        return None;
    }
    let map = map_file(&file)?;
    Some(Arc::new(MappedFont { map, _file: file }))
}

#[allow(unsafe_code)]
fn map_file(file: &File) -> Option<memmap2::Mmap> {
    // SAFETY: memmap2 requires that the file is not modified while mapped.
    // The handle was opened without FILE_SHARE_WRITE (Windows) and is kept
    // alive next to the mapping in `MappedFont`, so no other process can
    // open the font for writing while we use it; files under
    // %WINDIR%\Fonts are additionally owned by TrustedInstaller. Same
    // reasoning as fastpdf-core's document loader (ADR 0006).
    unsafe { memmap2::Mmap::map(file) }.ok()
}

#[cfg(windows)]
fn open_deny_write(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ: other readers are fine, writers are refused.
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(not(windows))]
fn open_deny_write(path: &std::path::Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro::hayro_interpret::hayro_cmap::CharacterCollection;

    fn query(name: &str, family: CidFamily, bold: bool) -> FallbackFontQuery {
        FallbackFontQuery {
            post_script_name: Some(name.to_owned()),
            is_bold: bold,
            character_collection: Some(CharacterCollection {
                family,
                supplement: 0,
            }),
            ..FallbackFontQuery::default()
        }
    }

    #[test]
    fn traditional_chinese_ming_maps_to_pmingliu_then_jhenghei() {
        let c = cjk_candidates(&query("MSung-Light", CidFamily::AdobeCNS1, false));
        assert_eq!(c[0], Candidate::new("mingliu.ttc", 1));
        assert!(c.contains(&Candidate::new("msjh.ttc", 0)));
        let c = cjk_candidates(&query("MHei-Medium", CidFamily::AdobeCNS1, false));
        assert_eq!(c[0], Candidate::new("msjh.ttc", 0));
        let c = cjk_candidates(&query("DFKaiShu-SB-Estd-BF", CidFamily::AdobeCNS1, false));
        assert_eq!(c[0], Candidate::new("kaiu.ttf", 0));
    }

    #[test]
    fn other_collections_have_candidates() {
        let gb = cjk_candidates(&query("STSong-Light", CidFamily::AdobeGB1, false));
        assert_eq!(gb[0].file, "simsun.ttc");
        let jp = cjk_candidates(&query("HeiseiKakuGo-W5", CidFamily::AdobeJapan1, false));
        assert_eq!(jp[0].file, "YuGothM.ttc");
        let jp_min = cjk_candidates(&query("HeiseiMin-W3", CidFamily::AdobeJapan1, false));
        assert!(jp_min.iter().any(|c| c.file == "msgothic.ttc"));
        let kr = cjk_candidates(&query("HYGoThic-Medium", CidFamily::AdobeKorea1, false));
        assert_eq!(kr[0].file, "malgun.ttf");
    }

    #[test]
    fn non_cjk_fallbacks_use_names() {
        let q = FallbackFontQuery {
            post_script_name: Some("Arial,Bold".into()),
            ..FallbackFontQuery::default()
        };
        assert!(cjk_candidates(&q).is_empty());
        assert_eq!(named_candidates(&q)[0].file, "arial.ttf");
    }

    #[test]
    fn ttc_header_is_parsed() {
        let mut ttc = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x04".to_vec();
        ttc.extend_from_slice(&[0; 16]);
        assert_eq!(ttc_face_count(&ttc), 4);
        assert_eq!(ttc_face_count(b"\x00\x01\x00\x00"), 1);
    }

    #[test]
    fn standard_fonts_always_resolve() {
        for font in [StandardFont::Helvetica, StandardFont::Symbol] {
            assert!(resolve(&FontQuery::Standard(font)).is_some());
        }
    }
}
