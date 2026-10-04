# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "reportlab==5.0.1",
#     "pypdf==6.19.0",
#     "pillow==12.3.0",
#     "fonttools==4.66.1",
#     "cryptography==50.0.2",
# ]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"
# ///
"""FastPDF benchmark / test fixture generator (docs/SPEC.md §27).

    uv run tools/fixtures/generate.py [--profile quick|full] [--out fixtures/generated]
                                      [--large-file-mb N] [--jobs N] [--only CAT,...]
                                      [--verify]

Output is deterministic: fixed seeds, reportlab ``invariant=1``, fixed
metadata dates, seeded encryption salts. See fixtures/README.md.
"""

from __future__ import annotations

import os
import sys

# Keep tools/fixtures free of __pycache__ (also inherited by worker processes).
sys.dont_write_bytecode = True
os.environ.setdefault("PYTHONDONTWRITEBYTECODE", "1")

import argparse  # noqa: E402
import json  # noqa: E402
import platform  # noqa: E402
import time  # noqa: E402
import traceback  # noqa: E402
import zlib  # noqa: E402
from concurrent.futures import ProcessPoolExecutor, as_completed  # noqa: E402
from importlib import metadata  # noqa: E402
from pathlib import Path  # noqa: E402
from typing import Any  # noqa: E402

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
DEFAULT_OUT = REPO / "fixtures" / "generated"
TRACKED_MANIFEST = REPO / "fixtures" / "manifest.quick.json"
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

CATEGORIES = ("small-text", "large-text", "scanned", "image-heavy", "vector-heavy", "cad",
              "fonts", "cjk", "japanese", "traditional-chinese", "transparency", "encrypted",
              "malformed", "large-page-count", "large-file")
_RESULT_KEYS = {"pages", "encryption", "features", "system_font"}


def all_jobs() -> list:
    from fxlib import cat_cjk, cat_encrypted, cat_malformed, cat_raster, cat_text, cat_vector

    jobs = []
    for mod in (cat_text, cat_raster, cat_vector, cat_cjk, cat_encrypted, cat_malformed):
        jobs.extend(mod.jobs())
    paths = [j.path for j in jobs]
    dupes = {p for p in paths if paths.count(p) > 1}
    if dupes:
        raise SystemExit(f"duplicate fixture paths: {sorted(dupes)}")
    for j in jobs:
        if j.category not in CATEGORIES:
            raise SystemExit(f"unknown category for {j.path}")
    return jobs


def run_job(job, out_dir: str, ctx) -> tuple[dict[str, Any], float]:
    """Executed in a worker process: generate one fixture, hash it, describe it."""
    import fxlib.rl  # noqa: F401  (reportlab determinism switches)
    from fxlib.common import SkipFixture, sha256_file

    final = Path(out_dir) / job.path
    final.parent.mkdir(parents=True, exist_ok=True)
    tmp = final.with_name(final.name + ".partial")
    t0 = time.perf_counter()
    try:
        result = job.func(tmp, ctx, **job.kwargs) or {}
    except SkipFixture as exc:
        tmp.unlink(missing_ok=True)
        final.unlink(missing_ok=True)
        return ({"skipped": True, "path": job.path, "category": job.category,
                 "reason": str(exc)}, time.perf_counter() - t0)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    os.replace(tmp, final)
    size, digest = sha256_file(final)
    pages = result.get("pages", job.pages)
    if job.pages is not None and pages != job.pages:
        raise RuntimeError(f"{job.path}: generated {pages} pages, job declares {job.pages}")
    rec: dict[str, Any] = {
        "path": job.path,
        "category": job.category,
        "bytes": size,
        "sha256": digest,
        "pages": pages,
        "encrypted": bool(result.get("encryption")),
        "encryption": result.get("encryption"),
        # password needed to open the file (top level for simple consumers such as fastpdf-bench)
        "password": (result.get("encryption") or {}).get("user_password"),
        "expected": job.expected,
        "description": job.description,
        "guardrails": list(job.guardrails),
        "features": list(job.features) + list(result.get("features", [])),
        "producer": job.producer,
    }
    if result.get("system_font"):
        rec["inputs"] = {"system_font": result["system_font"]}
    stats = {k: v for k, v in result.items() if k not in _RESULT_KEYS}
    if stats:
        rec["stats"] = stats
    return rec, time.perf_counter() - t0


def environment() -> dict[str, Any]:
    env: dict[str, Any] = {"python": platform.python_version(),
                           "platform": sys.platform, "zlib": zlib.ZLIB_RUNTIME_VERSION}
    for dist in ("reportlab", "pypdf", "pillow", "fonttools", "cryptography"):
        try:
            env[dist] = metadata.version(dist)
        except metadata.PackageNotFoundError:
            env[dist] = None
    return env


def remove_stale(out: Path, categories: set[str], keep: set[str]) -> list[str]:
    removed = []
    for cat in sorted(categories):
        d = out / cat
        if not d.is_dir():
            continue
        for p in sorted(d.iterdir()):
            rel = f"{cat}/{p.name}"
            if p.is_file() and (p.suffix in (".pdf", ".partial")) and rel not in keep:
                p.unlink()
                removed.append(rel)
    return removed


def write_json(path: Path, data: Any) -> None:
    tmp = path.with_name(path.name + ".partial")
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")
    os.replace(tmp, path)


def main(argv: list[str] | None = None) -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--profile", choices=("quick", "full"), default="quick")
    ap.add_argument("--out", default=None, help="output directory (default: fixtures/generated)")
    ap.add_argument("--large-file-mb", type=int, default=800,
                    help="target size of large-file fixture, full profile only (default 800)")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4,
                    help="worker processes (default: CPU count)")
    ap.add_argument("--only", default="", help="comma separated categories (development aid)")
    ap.add_argument("--verify", action="store_true",
                    help="run verify.py checks (pypdf) after generation")
    args = ap.parse_args(argv)

    from fxlib.common import EXPECTED_VALUES, GENERATOR_VERSION, SEED, Ctx
    from fxlib.fonts import discover_fonts

    out = Path(args.out).resolve() if args.out else DEFAULT_OUT
    only = {c.strip() for c in args.only.split(",") if c.strip()}
    unknown = only - set(CATEGORIES)
    if unknown:
        ap.error(f"unknown categories: {sorted(unknown)}")
    jobs = [j for j in all_jobs()
            if (args.profile == "full" or not j.full_only) and (not only or j.category in only)]
    jobs.sort(key=lambda j: -j.cost)  # longest first for a shorter makespan

    t_start = time.perf_counter()
    fonts, font_report = discover_fonts()
    ctx = Ctx(profile=args.profile, large_file_mb=args.large_file_mb, fonts=fonts)
    out.mkdir(parents=True, exist_ok=True)
    print(f"[fixtures] profile={args.profile} jobs={len(jobs)} workers={args.jobs} out={out}")
    for row in font_report:
        print(f"[fixtures] font role {row['role']:<12} {row['status']:<8} "
              f"{row.get('file', '')} {row.get('family', '')}")

    records: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    failures: list[str] = []
    with ProcessPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        futs = {ex.submit(run_job, j, str(out), ctx): j for j in jobs}
        for fut in as_completed(futs):
            job = futs[fut]
            try:
                rec, secs = fut.result()
            except Exception:
                failures.append(job.path)
                print(f"[fixtures] FAILED {job.path}\n{traceback.format_exc()}")
                continue
            if rec.get("skipped"):
                skipped.append({k: rec[k] for k in ("path", "category", "reason")})
                print(f"[fixtures] skip   {job.path}: {rec['reason']}")
            else:
                records.append(rec)
                print(f"[fixtures] ok     {secs:6.1f}s {rec['bytes'] / 1e6:9.2f} MB  {job.path}")
    if failures:
        print(f"[fixtures] {len(failures)} fixture(s) failed; manifest not written")
        return 1

    records.sort(key=lambda r: r["path"])
    skipped.sort(key=lambda r: r["path"])
    produced = {r["path"] for r in records}
    cats_run = {j.category for j in jobs}
    for rel in remove_stale(out, cats_run, produced):
        print(f"[fixtures] removed stale {rel}")

    by_cat: dict[str, dict[str, int]] = {}
    for r in records:
        c = by_cat.setdefault(r["category"], {"files": 0, "bytes": 0})
        c["files"] += 1
        c["bytes"] += r["bytes"]
    manifest = {
        "schema": "fastpdf-fixtures-manifest/1",
        "generator": {"script": "tools/fixtures/generate.py", "version": GENERATOR_VERSION,
                      "seed": SEED},
        "profile": args.profile,
        "options": {"large_file_mb": args.large_file_mb if args.profile == "full" else None,
                    "only": sorted(only) or None},
        "environment": environment(),
        "system_fonts": font_report,
        "expected_values": EXPECTED_VALUES,
        "summary": {"files": len(records), "bytes": sum(r["bytes"] for r in records),
                    "skipped": len(skipped),
                    "by_category": {k: by_cat[k] for k in CATEGORIES if k in by_cat}},
        "skipped": skipped,
        "files": records,
    }
    write_json(out / "manifest.json", manifest)
    if args.profile == "quick" and not only and out == DEFAULT_OUT:
        write_json(TRACKED_MANIFEST, manifest)
        print(f"[fixtures] wrote {TRACKED_MANIFEST.relative_to(REPO).as_posix()}")
    elapsed = time.perf_counter() - t_start
    print(f"[fixtures] {len(records)} files, {manifest['summary']['bytes'] / 1e6:.1f} MB, "
          f"{len(skipped)} skipped, {elapsed:.1f}s")

    if args.verify:
        import verify

        return verify.verify(out / "manifest.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
