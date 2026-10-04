"""Category: encrypted (pypdf Standard security handler, deterministic)."""

from __future__ import annotations

import contextlib
import io
from pathlib import Path
from typing import Iterator

from .common import Ctx, Job, latin_paragraph, rng
from .imaging import jpeg, synth_photo
from .rl import A4, wrap_text


@contextlib.contextmanager
def deterministic_secrets(key: str) -> Iterator[None]:
    """pypdf draws salts, IVs and file keys from ``secrets.token_bytes``.

    Replace it with a seeded generator while writing so that the encrypted
    output is byte-for-byte reproducible (fixtures only, never for real use).
    """
    import secrets

    r = rng("secrets:" + key)
    original = secrets.token_bytes
    secrets.token_bytes = lambda n=32: r.randbytes(32 if n is None else n)
    try:
        yield
    finally:
        secrets.token_bytes = original


def _base_document() -> bytes:
    from reportlab.lib.utils import ImageReader
    from reportlab.pdfgen.canvas import Canvas

    r = rng("encrypted-base")
    buf = io.BytesIO()
    c = Canvas(buf, pagesize=A4, invariant=1, pageCompression=1)
    c.setTitle("加密測試文件 Encrypted fixture")
    c.setSubject("字串也會被加密 (strings are encrypted too)")
    c.setAuthor("FastPDF fixture generator (synthetic content)")
    W, H = A4
    c.setFont("Helvetica-Bold", 20)
    c.drawString(60, H - 80, "Encrypted fixture - page 1: text")
    c.setFont("Helvetica", 11)
    y = H - 110
    for _ in range(6):
        for line in wrap_text(latin_paragraph(r), "Helvetica", 11, W - 120):
            c.drawString(60, y, line)
            y -= 15
        y -= 6
    c.linkURL("https://example.com/fastpdf-encrypted", (60, 60, 300, 80), relative=0)
    c.drawString(60, 66, "URI annotation (encrypted string)")
    c.showPage()
    c.setFont("Helvetica-Bold", 20)
    c.drawString(60, H - 80, "Encrypted fixture - page 2: images")
    c.drawImage(ImageReader(io.BytesIO(jpeg(synth_photo(r, 900, 600), 85))), 60, H - 520,
                width=475, height=317)
    c.showPage()
    c.setFont("Helvetica-Bold", 20)
    c.drawString(60, H - 80, "Encrypted fixture - page 3: vectors")
    for i in range(200):
        c.setStrokeColorRGB(i / 200, 0.3, 1 - i / 200)
        c.line(60 + i * 2.3, 100, 300 - i, 600 + (i % 20) * 5)
    c.showPage()
    c.save()
    return buf.getvalue()


def encrypted(out: Path, ctx: Ctx, algorithm: str, user_password: str, owner_password: str,
              print_only: bool) -> dict:
    from pypdf import PdfReader, PdfWriter
    from pypdf.constants import UserAccessPermissions as P

    base = _base_document()
    perms = (P.PRINT | P.PRINT_TO_REPRESENTATION) if print_only else None
    key = f"{algorithm}:{user_password}:{owner_password}"
    with deterministic_secrets(key):
        writer = PdfWriter(clone_from=PdfReader(io.BytesIO(base)))
        kwargs = {"algorithm": algorithm}
        if perms is not None:
            kwargs["permissions_flag"] = perms
        writer.encrypt(user_password=user_password, owner_password=owner_password, **kwargs)
        with open(out, "wb") as f:
            writer.write(f)
    V, R, bits = {"RC4-40": (1, 2, 40), "RC4-128": (2, 3, 128), "AES-128": (4, 4, 128),
                  "AES-256-R5": (5, 5, 256), "AES-256": (5, 6, 256)}[algorithm]
    return {"pages": 3, "encryption": {
        "handler": "Standard", "algorithm": algorithm, "V": V, "R": R, "key_bits": bits,
        "user_password": user_password, "owner_password": owner_password,
        "permissions": "print-only" if print_only else "all"}}


def _job(name: str, algorithm: str, user: str, owner: str, print_only: bool, desc: str) -> Job:
    return Job(f"encrypted/{name}.pdf", encrypted,
               "password_required" if user else "open_ok", desc, pages=3,
               features=("Encrypt", algorithm) + (("user-password",) if user else ("owner-only",)),
               kwargs={"algorithm": algorithm, "user_password": user, "owner_password": owner,
                       "print_only": print_only},
               producer="pypdf", cost=0.4)


def jobs() -> list[Job]:
    return [
        _job("aes256-r6-owner-only", "AES-256", "", "fastpdf-owner-aes256", True,
             "AES-256（V5／R6，PDF 2.0 / ISO 32000-2），空 user password、有 owner password，權限僅允許列印。不需密碼即可開啟。"),
        _job("aes256-r5-owner-only", "AES-256-R5", "", "fastpdf-owner-r5", True,
             "AES-256（V5／R5，Acrobat 9 已棄用的擴充），空 user password。"),
        _job("aes128-owner-only", "AES-128", "", "fastpdf-owner-aes128", True,
             "AES-128（V4／R4，AESV2 crypt filter），空 user password。"),
        _job("rc4-128-owner-only", "RC4-128", "", "fastpdf-owner-rc4", True,
             "RC4 128-bit（V2／R3），空 user password。"),
        _job("rc4-40-owner-only", "RC4-40", "", "fastpdf-owner-rc4-40", False,
             "RC4 40-bit（V1／R2，舊式），空 user password。"),
        _job("aes256-r6-user-password", "AES-256", "fastpdf-user", "fastpdf-owner", False,
             "AES-256（R6），需要 user password 才能開啟（密碼見 encryption.user_password）。"),
        _job("aes256-r6-user-password-unicode", "AES-256", "測試密碼2026", "fastpdf-owner-unicode",
             False, "AES-256（R6），user password 含中文（UTF-8／SASLprep），測 Unicode 密碼處理。"),
        _job("aes128-user-password", "AES-128", "fastpdf-user128", "fastpdf-owner128", False,
             "AES-128（R4），需要 user password（MD5 金鑰推導路徑）。"),
    ]
