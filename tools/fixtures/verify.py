# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "pypdf==6.19.0",
#     "cryptography==50.0.2",
# ]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"
# ///
"""Verify generated fixtures against their manifest using pypdf.

    uv run tools/fixtures/verify.py [--manifest fixtures/generated/manifest.json]
                                    [--no-hash] [--no-probe] [--timeout 60]

* every file: exists, size and sha256 match the manifest
* non-malformed files: pypdf opens them (decrypting when needed) and the page
  count matches; password_required files must refuse the empty password
* malformed files: opened by pypdf in a subprocess with a timeout; the outcome
  is reported for information only (pypdf is a reference, not the oracle)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.dont_write_bytecode = True
DEFAULT_MANIFEST = Path(__file__).resolve().parents[2] / "fixtures" / "generated" / "manifest.json"


def _sha256(path: Path) -> tuple[int, str]:
    h = hashlib.sha256()
    n = 0
    with open(path, "rb") as f:
        while chunk := f.read(1 << 22):
            n += len(chunk)
            h.update(chunk)
    return n, h.hexdigest()


def check_valid(path: Path, rec: dict) -> list[str]:
    import warnings

    from pypdf import PasswordType, PdfReader

    errors: list[str] = []
    warnings.simplefilter("ignore")
    reader = PdfReader(path, strict=False)
    enc = rec.get("encryption")
    if rec.get("encrypted"):
        if not reader.is_encrypted:
            return ["manifest says encrypted, pypdf says not encrypted"]
        if enc["user_password"]:
            if reader.decrypt("") != PasswordType.NOT_DECRYPTED:
                errors.append("opened with an empty password")
            reader = PdfReader(path, strict=False)
            if reader.decrypt(enc["user_password"]) == PasswordType.NOT_DECRYPTED:
                errors.append("user password rejected")
        elif reader.decrypt("") == PasswordType.NOT_DECRYPTED:
            errors.append("empty user password rejected")
        owner = PdfReader(path, strict=False)
        if owner.decrypt(enc["owner_password"]) != PasswordType.OWNER_PASSWORD:
            errors.append("owner password not recognised")
    elif reader.is_encrypted:
        errors.append("unexpectedly encrypted")
    n = len(reader.pages)
    if rec.get("pages") is not None and n != rec["pages"]:
        errors.append(f"page count {n} != manifest {rec['pages']}")
    for idx in {0, n - 1}:
        box = reader.pages[idx].mediabox
        if box.width <= 0 or box.height <= 0:
            errors.append(f"page {idx + 1} has an empty MediaBox")
    return errors


_PROBE = r"""
import sys, json, warnings
warnings.simplefilter("ignore")
sys.setrecursionlimit(10000)
from pypdf import PdfReader
try:
    r = PdfReader(sys.argv[1], strict=False)
    n = len(r.pages)
    print(json.dumps({"outcome": "opened", "pages": n}))
except BaseException as e:
    print(json.dumps({"outcome": "error", "error": f"{type(e).__name__}: {str(e)[:120]}"}))
"""


def probe(path: Path, timeout: float) -> dict:
    try:
        cp = subprocess.run([sys.executable, "-c", _PROBE, str(path)], capture_output=True,
                            text=True, timeout=timeout)
        line = (cp.stdout.strip().splitlines() or [""])[-1]
        return json.loads(line) if line.startswith("{") else {
            "outcome": "crash", "error": (cp.stderr.strip().splitlines() or ["?"])[-1][:120]}
    except subprocess.TimeoutExpired:
        return {"outcome": "timeout", "error": f">{timeout:.0f}s"}


def verify(manifest_path: Path, *, check_hash: bool = True, run_probe: bool = True,
           timeout: float = 60.0) -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
    root = Path(manifest_path).parent
    failures = 0
    malformed = []
    for rec in manifest["files"]:
        path = root / rec["path"]
        errs: list[str] = []
        if not path.is_file():
            errs.append("missing file")
        elif check_hash:
            size, digest = _sha256(path)
            if size != rec["bytes"]:
                errs.append(f"size {size} != {rec['bytes']}")
            if digest != rec["sha256"]:
                errs.append("sha256 mismatch")
        if not errs:
            if rec["category"] == "malformed":
                malformed.append(rec)
            else:
                try:
                    errs += check_valid(path, rec)
                except Exception as exc:  # pypdf failure on a file that must be valid
                    errs.append(f"pypdf: {type(exc).__name__}: {exc}")
        status = "FAIL" if errs else "ok"
        if rec["category"] != "malformed" or errs:
            print(f"[verify] {status:4} {rec['path']}" + (f"  -> {'; '.join(errs)}" if errs else ""))
        failures += bool(errs)
    if run_probe and malformed:
        with ThreadPoolExecutor(max_workers=8) as ex:
            results = list(ex.map(lambda r: probe(root / r["path"], timeout), malformed))
        for rec, res in zip(malformed, results):
            detail = (f"pages={res['pages']}" if res["outcome"] == "opened"
                      else res.get("error", ""))
            print(f"[verify] info {rec['path']}  expected={rec['expected']}  "
                  f"pypdf={res['outcome']} {detail}")
    checked = len(manifest["files"])
    print(f"[verify] {checked} files checked, {failures} failure(s), "
          f"{len(malformed)} malformed probed (informational)")
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description="Verify FastPDF fixtures with pypdf")
    ap.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    ap.add_argument("--no-hash", action="store_true")
    ap.add_argument("--no-probe", action="store_true")
    ap.add_argument("--timeout", type=float, default=60.0)
    a = ap.parse_args()
    return verify(Path(a.manifest), check_hash=not a.no_hash, run_probe=not a.no_probe,
                  timeout=a.timeout)


if __name__ == "__main__":
    sys.exit(main())
