//! Test support: a tiny PDF writer (so tests do not depend on generated or
//! third-party fixtures), RC4 encryption for password tests, and helpers
//! around the guarded engine.

#![allow(dead_code)]
// each test binary uses a different subset
// Test support: failing loudly on broken test setup is the point.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineDocument, EngineError, GuardedDocument, OpenOptions,
    PageIndex, PixelFormat, PixelRect, Pixmap, RenderRequest, RenderScale, ResourceLimits,
    Rotation, SharedBytes, open_guarded,
};
use fastpdf_engine_hayro::HayroEngine;

/// Bytes of a generated fixture (`tools/fixtures/generate.py`), if present.
/// Generated fixtures are not committed; tests that use them skip cleanly.
pub(crate) fn fixture(rel: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/generated")
        .join(rel);
    let bytes = std::fs::read(&path).ok();
    if bytes.is_none() {
        eprintln!("skipping: fixture {} not generated", path.display());
    }
    bytes
}

pub(crate) fn open_with(
    bytes: Vec<u8>,
    password: Option<&str>,
    limits: ResourceLimits,
) -> Result<GuardedDocument, EngineError> {
    let options = OpenOptions {
        password: password.map(str::to_owned),
        limits,
    };
    open_guarded(
        &HayroEngine::new(),
        DocumentSource::from_bytes(SharedBytes::from_vec(bytes)),
        &options,
    )
}

pub(crate) fn open(bytes: Vec<u8>) -> GuardedDocument {
    open_with(bytes, None, ResourceLimits::default()).expect("document opens")
}

pub(crate) fn scale(s: f32) -> RenderScale {
    RenderScale::new(s).expect("valid scale")
}

pub(crate) fn full_request(
    doc: &GuardedDocument,
    page: u32,
    s: f32,
    rotation: Rotation,
) -> RenderRequest {
    let info = doc.page_info(PageIndex::new(page)).expect("page info");
    RenderRequest::full_page(
        PageIndex::new(page),
        info.size,
        info.rotation,
        rotation,
        scale(s),
    )
}

pub(crate) fn render(
    doc: &GuardedDocument,
    request: &RenderRequest,
    format: PixelFormat,
) -> Result<Pixmap, EngineError> {
    let mut pm = Pixmap::new(request.region.size(), format, doc.limits())?;
    doc.render(request, &mut pm.as_mut(), &CancelToken::new())?;
    Ok(pm)
}

pub(crate) fn render_region(
    doc: &GuardedDocument,
    request: &RenderRequest,
    region: PixelRect,
) -> Result<Pixmap, EngineError> {
    render(
        doc,
        &request.clone().with_region(region),
        PixelFormat::default(),
    )
}

/// RGBA of pixel (x, y).
pub(crate) fn pixel(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * pm.size().width as usize + x as usize) * 4;
    let d = pm.data();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

/// Number of pixels whose channels differ by more than `tolerance`.
pub(crate) fn differing_pixels(a: &[u8], b: &[u8], tolerance: u8) -> usize {
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(p, q)| {
            p.iter()
                .zip(q.iter())
                .any(|(x, y)| x.abs_diff(*y) > tolerance)
        })
        .count()
}

pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("in-memory write");
    e.finish().expect("in-memory write")
}

enum Obj {
    Raw(Vec<u8>),
    Stream { dict: String, data: Vec<u8> },
}

/// A minimal PDF writer: objects are numbered from 1 in insertion order.
#[derive(Default)]
pub(crate) struct PdfBuilder {
    objects: Vec<Option<Obj>>,
    encryption: Option<Rc4Encryption>,
}

impl PdfBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Reserves an object number to fill later with `set`/`set_stream`.
    pub(crate) fn reserve(&mut self) -> u32 {
        self.objects.push(None);
        self.objects.len() as u32
    }

    pub(crate) fn add(&mut self, body: impl Into<String>) -> u32 {
        let n = self.reserve();
        self.set(n, body);
        n
    }

    pub(crate) fn set(&mut self, n: u32, body: impl Into<String>) {
        self.objects[n as usize - 1] = Some(Obj::Raw(body.into().into_bytes()));
    }

    /// A stream object; `dict` is the dictionary body without `<< >>` and
    /// without `/Length`.
    pub(crate) fn add_stream(&mut self, dict: &str, data: Vec<u8>) -> u32 {
        let n = self.reserve();
        self.set_stream(n, dict, data);
        n
    }

    pub(crate) fn set_stream(&mut self, n: u32, dict: &str, data: Vec<u8>) {
        self.objects[n as usize - 1] = Some(Obj::Stream {
            dict: dict.to_owned(),
            data,
        });
    }

    /// Encrypts all streams with RC4-40 (Standard handler, R2).
    pub(crate) fn encrypt_rc4(&mut self, user: &str, owner: &str) {
        self.encryption = Some(Rc4Encryption::new(user.as_bytes(), owner.as_bytes()));
    }

    /// Serializes with a classic xref table; `root` is the catalog.
    pub(crate) fn finish(mut self, root: u32) -> Vec<u8> {
        let encrypt_obj = self.encryption.as_ref().map(|e| e.dictionary());
        let encrypt_ref = encrypt_obj.map(|body| self.add(body));
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in self.objects.iter().enumerate() {
            let n = i as u32 + 1;
            offsets.push(out.len());
            writeln!(out, "{n} 0 obj").ok();
            match obj {
                Some(Obj::Raw(body)) => out.extend_from_slice(body),
                Some(Obj::Stream { dict, data }) => {
                    let data = match &self.encryption {
                        Some(e) if Some(n) != encrypt_ref => e.encrypt(n, data),
                        _ => data.clone(),
                    };
                    write!(out, "<< {dict} /Length {} >>\nstream\n", data.len()).ok();
                    out.extend_from_slice(&data);
                    out.extend_from_slice(b"\nendstream");
                }
                None => out.extend_from_slice(b"null"),
            }
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let size = self.objects.len() + 1;
        write!(out, "xref\n0 {size}\n0000000000 65535 f \n").ok();
        for o in offsets {
            writeln!(out, "{o:010} 00000 n ").ok();
        }
        let extra = match (&self.encryption, encrypt_ref) {
            (Some(e), Some(r)) => format!(" /Encrypt {r} 0 R /ID [<{0}> <{0}>]", hex(&e.id)),
            _ => String::new(),
        };
        write!(
            out,
            "trailer\n<< /Size {size} /Root {root} 0 R{extra} >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .ok();
        out
    }
}

/// One page of a synthetic document.
pub(crate) struct PageSpec {
    pub(crate) media_box: [f64; 4],
    pub(crate) crop_box: Option<[f64; 4]>,
    pub(crate) rotate: Option<i64>,
    pub(crate) user_unit: Option<f64>,
    pub(crate) content: String,
    /// Extra entries for the page's /Resources dictionary.
    pub(crate) resources: String,
    /// Extra entries for the page dictionary.
    pub(crate) extra: String,
}

impl PageSpec {
    pub(crate) fn new(media_box: [f64; 4], content: &str) -> Self {
        Self {
            media_box,
            crop_box: None,
            rotate: None,
            user_unit: None,
            content: content.to_owned(),
            resources: String::new(),
            extra: String::new(),
        }
    }
}

fn rect(r: [f64; 4]) -> String {
    format!("[{} {} {} {}]", r[0], r[1], r[2], r[3])
}

/// Builds a document from page specs. Font `/F1` (Helvetica) is available
/// on every page.
pub(crate) fn build(pages: &[PageSpec]) -> Vec<u8> {
    build_with(pages, |_| {})
}

/// Like `build`, with a hook to add objects before serialization (the
/// catalog is object 1, the page tree object 2).
pub(crate) fn build_with(pages: &[PageSpec], hook: impl FnOnce(&mut PdfBuilder)) -> Vec<u8> {
    build_full(pages, "", hook)
}

pub(crate) fn build_full(
    pages: &[PageSpec],
    catalog_extra: &str,
    hook: impl FnOnce(&mut PdfBuilder),
) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let catalog = b.reserve();
    let tree = b.reserve();
    let font = b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let mut kids = Vec::new();
    for page in pages {
        let content = b.add_stream("", page.content.as_bytes().to_vec());
        let mut dict = format!(
            "<< /Type /Page /Parent {tree} 0 R /MediaBox {} /Contents {content} 0 R /Resources << /Font << /F1 {font} 0 R >> {} >>",
            rect(page.media_box),
            page.resources
        );
        if let Some(c) = page.crop_box {
            dict.push_str(&format!(" /CropBox {}", rect(c)));
        }
        if let Some(r) = page.rotate {
            dict.push_str(&format!(" /Rotate {r}"));
        }
        if let Some(u) = page.user_unit {
            dict.push_str(&format!(" /UserUnit {u}"));
        }
        dict.push_str(&format!(" {} >>", page.extra));
        kids.push(b.add(dict));
    }
    let kids_list: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    b.set(
        tree,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids_list.join(" "),
            kids.len()
        ),
    );
    b.set(
        catalog,
        format!("<< /Type /Catalog /Pages {tree} 0 R {catalog_extra} >>"),
    );
    hook(&mut b);
    b.finish(catalog)
}

/// Object number of page `i` (0-based) in documents made by `build*`:
/// catalog, tree, font, then (content, page) pairs.
pub(crate) fn page_object(i: u32) -> u32 {
    5 + 2 * i
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

// --- RC4-40 (Standard security handler, revision 2) ------------------------

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

struct Rc4Encryption {
    key: Vec<u8>,
    o: Vec<u8>,
    u: Vec<u8>,
    id: Vec<u8>,
}

const PERMISSIONS: i32 = -4;

fn padded(password: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = password.iter().copied().take(32).collect();
    v.extend_from_slice(&PAD[..32 - v.len()]);
    v
}

impl Rc4Encryption {
    fn new(user: &[u8], owner: &[u8]) -> Self {
        let id = b"fastpdf-test-id!".to_vec();
        // Algorithm 3: O entry.
        let owner_key = md5(&padded(if owner.is_empty() { user } else { owner }));
        let o = rc4(&owner_key[..5], &padded(user));
        // Algorithm 2: file key.
        let mut input = padded(user);
        input.extend_from_slice(&o);
        input.extend_from_slice(&PERMISSIONS.to_le_bytes());
        input.extend_from_slice(&id);
        let key = md5(&input)[..5].to_vec();
        // Algorithm 4: U entry.
        let u = rc4(&key, &PAD);
        Self { key, o, u, id }
    }

    fn dictionary(&self) -> String {
        format!(
            "<< /Filter /Standard /V 1 /R 2 /Length 40 /P {PERMISSIONS} /O <{}> /U <{}> >>",
            hex(&self.o),
            hex(&self.u)
        )
    }

    fn encrypt(&self, obj: u32, data: &[u8]) -> Vec<u8> {
        let mut input = self.key.clone();
        input.extend_from_slice(&obj.to_le_bytes()[..3]);
        input.extend_from_slice(&[0, 0]);
        let k = md5(&input);
        rc4(&k[..(self.key.len() + 5).min(16)], data)
    }
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// RFC 1321 MD5 (test-only; used for the PDF key derivation).
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in msg.as_chunks::<64>().0 {
        let m: Vec<u32> = chunk
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

#[test]
fn md5_matches_rfc_1321() {
    assert_eq!(hex(&md5(b"")), "D41D8CD98F00B204E9800998ECF8427E");
    assert_eq!(
        hex(&md5(b"The quick brown fox jumps over the lazy dog")),
        "9E107D9D372BB6826BD81D3542A419D6"
    );
}
