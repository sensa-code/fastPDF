//! The fonts text edits draw new text with (ADR 0013): a TrueType face of
//! a font file on the system, embedded as a subset.
//!
//! New text is written with a Type0 font whose CIDFontType2 descendant
//! uses `Identity-H` and `/CIDToGIDMap /Identity`, so a character code is
//! the two-byte glyph id in the subset (typst's `subsetter` numbers the
//! glyphs it keeps from 1 on in the order they were first used; `.notdef`
//! stays 0). Widths come from the face's horizontal metrics, and a
//! ToUnicode map gives every glyph its character so the new text can be
//! selected, searched and copied. Characters map one to one through the
//! face's `cmap`, without shaping, which CJK text and plain Latin text do
//! not need.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use fastpdf_engine_api::write::FontFace;
use hayro::hayro_interpret::font::FontData;
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};
use subsetter::GlyphRemapper;

use super::syntax::real;
use crate::fonts;

/// OS/2 `fsType` (OpenType): the usage permission in bits 0-3, and two
/// flags.
const PERMISSION_MASK: u16 = 0x000F;
const RESTRICTED_LICENSE: u16 = 0x0002;
const NO_SUBSETTING: u16 = 0x0100;
const BITMAP_ONLY: u16 = 0x0200;
/// FontDescriptor flags (PDF 32000-1, table 123).
const FLAG_FIXED_PITCH: u32 = 1;
const FLAG_SYMBOLIC: u32 = 4;
const FLAG_ITALIC: u32 = 64;
/// Entries per `beginbfchar` block (the CMap format's limit).
const BFCHAR_BLOCK: usize = 100;

/// A font face new text is drawn with, and the glyphs used so far.
pub(crate) struct EditFont {
    data: FontData,
    index: u32,
    /// Font design units per em.
    units_per_em: f64,
    remapper: GlyphRemapper,
    /// Every glyph used (by its id in the subset): its character and its
    /// advance in glyph space units (1000 per em).
    used: BTreeMap<u16, (char, f64)>,
    /// The character each used glyph (by its id in the face) stands for.
    owner: HashMap<u16, char>,
}

/// New text as character codes for the font.
pub(crate) struct Encoded {
    /// Two bytes per character.
    pub(crate) codes: Vec<u8>,
    /// Each character's advance in glyph space units (1000 per em).
    pub(crate) advances: Vec<f64>,
}

/// What the document gets for a font: the pieces of its objects.
pub(crate) struct Embedded {
    /// `/BaseFont` and `/FontName`: a subset tag and the face's PostScript
    /// name.
    pub(crate) base_font: Vec<u8>,
    /// The CIDFont's `/W` array.
    pub(crate) widths: String,
    /// FontDescriptor entries other than `/FontName` and `/FontFile2`.
    pub(crate) descriptor: String,
    /// The subset font program.
    pub(crate) program: Vec<u8>,
    /// The ToUnicode CMap.
    pub(crate) to_unicode: Vec<u8>,
}

impl EditFont {
    /// Opens `face`. Refuses files that are not TrueType faces and fonts
    /// whose license does not allow embedding a subset.
    pub(crate) fn open(face: &FontFace) -> Result<Self, String> {
        let data =
            fonts::open_font(&face.path).ok_or_else(|| "cannot read the font file".to_owned())?;
        let font = FontRef::from_index((*data).as_ref(), face.index)
            .map_err(|e| format!("not a usable font face: {e}"))?;
        if font.glyf().is_err() || font.loca(None).is_err() {
            return Err("only TrueType outlines can be embedded".into());
        }
        let fs_type = font.os2().map(|t| t.fs_type()).unwrap_or(0);
        if fs_type & PERMISSION_MASK == RESTRICTED_LICENSE {
            return Err("the font's license does not allow embedding".into());
        }
        if fs_type & (NO_SUBSETTING | BITMAP_ONLY) != 0 {
            return Err("the font's license does not allow embedding a subset".into());
        }
        let units_per_em = font
            .head()
            .map(|h| h.units_per_em())
            .ok()
            .filter(|&u| u > 0)
            .ok_or_else(|| "the font has no units per em".to_owned())?;
        Ok(Self {
            data,
            index: face.index,
            units_per_em: f64::from(units_per_em),
            remapper: GlyphRemapper::new(),
            used: BTreeMap::new(),
            owner: HashMap::new(),
        })
    }

    /// `text` as codes of this font. Fails with the characters the face has
    /// no glyph for (or shares one glyph with another character, which a
    /// ToUnicode map cannot tell apart), each once.
    pub(crate) fn encode(&mut self, text: &str) -> Result<Encoded, String> {
        let data = FontData::clone(&self.data);
        let font = FontRef::from_index((*data).as_ref(), self.index).map_err(|e| e.to_string())?;
        let charmap = font.charmap();
        let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        let mut glyphs = Vec::with_capacity(text.len());
        let mut missing = String::new();
        // Glyphs this text adds, with their characters.
        let mut added: HashMap<u16, char> = HashMap::new();
        for c in text.chars() {
            let glyph = charmap
                .map(c)
                .and_then(|g| u16::try_from(g.to_u32()).ok())
                .filter(|&g| g != 0);
            let owner = glyph.and_then(|g| self.owner.get(&g).or(added.get(&g)).copied());
            match glyph {
                Some(g) if owner.is_none_or(|o| o == c) => {
                    added.insert(g, c);
                    glyphs.push((c, g));
                }
                _ if !missing.contains(c) => missing.push(c),
                _ => {}
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        let scale = 1000.0 / self.units_per_em;
        let mut encoded = Encoded {
            codes: Vec::with_capacity(glyphs.len() * 2),
            advances: Vec::with_capacity(glyphs.len()),
        };
        for (c, g) in glyphs {
            let new = self.remapper.remap(g);
            let advance = f64::from(
                metrics
                    .advance_width(skrifa::GlyphId::from(g))
                    .unwrap_or(0.0),
            ) * scale;
            self.owner.insert(g, c);
            self.used.entry(new).or_insert((c, advance));
            encoded.codes.extend_from_slice(&new.to_be_bytes());
            encoded.advances.push(advance);
        }
        Ok(encoded)
    }

    /// Whether any text was encoded with the font (only then is it
    /// embedded).
    pub(crate) fn is_used(&self) -> bool {
        !self.used.is_empty()
    }

    /// The font's objects for the glyphs used.
    pub(crate) fn embed(&self) -> Result<Embedded, String> {
        let font =
            FontRef::from_index((*self.data).as_ref(), self.index).map_err(|e| e.to_string())?;
        let program = subsetter::subset((*self.data).as_ref(), self.index, &self.remapper)
            .map_err(|e| format!("cannot subset the font: {e}"))?;
        let scale = 1000.0 / self.units_per_em;
        let mut base_font = subset_tag(&self.remapper).into_bytes();
        base_font.push(b'+');
        base_font.extend_from_slice(&postscript_name(&font));

        let metrics = font.metrics(Size::unscaled(), LocationRef::default());
        let bbox = font.head().map_or([0.0, -200.0, 1000.0, 800.0], |h| {
            [
                f64::from(h.x_min()) * scale,
                f64::from(h.y_min()) * scale,
                f64::from(h.x_max()) * scale,
                f64::from(h.y_max()) * scale,
            ]
        });
        let ascent = f64::from(metrics.ascent) * scale;
        let descent = f64::from(metrics.descent) * scale;
        let cap_height = metrics.cap_height.map_or(ascent, |c| f64::from(c) * scale);
        let mut flags = FLAG_SYMBOLIC;
        if metrics.is_monospace {
            flags |= FLAG_FIXED_PITCH;
        }
        if metrics.italic_angle != 0.0 {
            flags |= FLAG_ITALIC;
        }
        let descriptor = format!(
            "/Flags {flags}/FontBBox [{}]/ItalicAngle {}/Ascent {}/Descent {}/CapHeight {}/StemV 80",
            bbox.map(real).join(" "),
            real(f64::from(metrics.italic_angle)),
            real(ascent),
            real(descent),
            real(cap_height),
        );
        Ok(Embedded {
            base_font,
            widths: self.widths(),
            descriptor,
            program,
            to_unicode: self.to_unicode(),
        })
    }

    /// The `/W` array: runs of consecutive glyph ids with their widths.
    fn widths(&self) -> String {
        let mut out = String::from("[");
        let mut run: Option<u16> = None;
        let mut previous = 0u16;
        for (&id, &(_, width)) in &self.used {
            match run {
                Some(_) if previous.checked_add(1) == Some(id) => out.push(' '),
                Some(_) => {
                    let _ = write!(out, "] {id} [");
                }
                None => {
                    let _ = write!(out, "{id} [");
                }
            }
            run = Some(id);
            previous = id;
            out.push_str(&real(width));
        }
        if run.is_some() {
            out.push(']');
        }
        out.push(']');
        out
    }

    fn to_unicode(&self) -> Vec<u8> {
        let mut out = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CIDSystemInfo <</Registry (Adobe) /Ordering (UCS) /Supplement 0>> def\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let entries: Vec<(&u16, &(char, f64))> = self.used.iter().collect();
        for block in entries.chunks(BFCHAR_BLOCK) {
            let _ = writeln!(out, "{} beginbfchar", block.len());
            for (id, (c, _)) in block {
                let mut units = [0u16; 2];
                let hex: String = c
                    .encode_utf16(&mut units)
                    .iter()
                    .map(|u| format!("{u:04X}"))
                    .collect();
                let _ = writeln!(out, "<{id:04X}> <{hex}>");
            }
            out.push_str("endbfchar\n");
        }
        out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        out.into_bytes()
    }
}

/// Six capital letters naming this subset (PDF 32000-1, 9.6.4), from the
/// glyphs it has.
fn subset_tag(remapper: &GlyphRemapper) -> String {
    // FNV-1a over the original glyph ids, in subset order.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for id in remapper.remapped_gids() {
        for b in id.to_be_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    (0..6)
        .map(|i| char::from(b'A' + ((hash >> (i * 8)) % 26) as u8))
        .collect()
}

/// The face's PostScript name, as characters a PDF name can hold without
/// escapes.
fn postscript_name(font: &FontRef<'_>) -> Vec<u8> {
    let name: Vec<u8> = font
        .localized_strings(StringId::POSTSCRIPT_NAME)
        .english_or_first()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_graphic() && !"()<>[]{}/%#".contains(*c))
                .map(|c| c as u8)
                .collect()
        })
        .unwrap_or_default();
    if name.is_empty() {
        b"FastPdfFont".to_vec()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kaiu() -> Option<EditFont> {
        let path = std::path::PathBuf::from(r"C:\Windows\Fonts\kaiu.ttf");
        path.exists()
            .then(|| EditFont::open(&FontFace { path, index: 0 }).expect("kaiu.ttf opens"))
    }

    #[test]
    fn text_is_encoded_with_subset_glyph_ids() {
        let Some(mut font) = kaiu() else {
            eprintln!("skipped: kaiu.ttf is not installed");
            return;
        };
        let first = font.encode("原告甲").unwrap();
        assert_eq!(first.codes, [0, 1, 0, 2, 0, 3]);
        assert!(first.advances.iter().all(|&a| (a - 1000.0).abs() < 1.0));
        // Glyphs keep their numbers; new ones continue the count.
        let second = font.encode("甲 A").unwrap();
        assert_eq!(second.codes, [0, 3, 0, 4, 0, 5]);
        assert_eq!(
            font.encode("原\u{10FFFD}").err().as_deref(),
            Some("\u{10FFFD}")
        );
        let embedded = font.embed().unwrap();
        assert!(embedded.program.len() < 64 * 1024, "a small subset");
        assert_eq!(embedded.base_font[6], b'+');
        assert!(embedded.base_font[..6].iter().all(u8::is_ascii_uppercase));
        assert!(embedded.widths.starts_with("[1 [1000 1000 1000 "));
        let cmap = String::from_utf8(embedded.to_unicode).unwrap();
        assert!(cmap.contains("5 beginbfchar\n<0001> <539F>\n"));
        assert!(cmap.contains("<0005> <0041>\n"));
        // The subset is a font hayro's font stack reads.
        let subset = FontRef::new(&embedded.program).unwrap();
        assert!(subset.maxp().unwrap().num_glyphs() >= 6);
    }

    #[test]
    fn widths_group_consecutive_ids() {
        let Some(mut font) = kaiu() else {
            return;
        };
        font.encode("ab").unwrap();
        font.used.insert(5, ('z', 250.0));
        let widths = font.widths();
        assert!(widths.starts_with("[1 ["), "{widths}");
        assert!(widths.ends_with("] 5 [250]]"), "{widths}");
    }

    #[test]
    fn unusable_files_are_refused() {
        let dir = std::env::temp_dir().join(format!("fastpdf-font-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.ttf");
        std::fs::write(&junk, b"not a font").unwrap();
        assert!(
            EditFont::open(&FontFace {
                path: junk,
                index: 0
            })
            .is_err()
        );
        assert!(
            EditFont::open(&FontFace {
                path: dir.join("missing.ttf"),
                index: 0
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(subset_tag(&GlyphRemapper::new()).len(), 6);
    }
}
