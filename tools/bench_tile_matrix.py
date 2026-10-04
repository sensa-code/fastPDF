#!/usr/bin/env python3
"""Benchmark plan B-3 / B-4: tile size x worker count (spec §12, §18).

Runs `fastpdf-bench render --tile T --workers W --viewport 1920x1080` for a
set of fixtures and zoom levels. Every measurement is a fresh process (cold
engine caches), repeated and reduced to the median. Prints a Markdown summary
and writes the raw results as JSON.

Usage:
    python tools/bench_tile_matrix.py --bench target/release/fastpdf-bench.exe
        [--repeat 3] [--out benchmarks/runs/b3b4.json]

Run on a quiet machine; results are only comparable on the same machine.
"""

from __future__ import annotations

import argparse
import itertools
import json
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = [
    "small-text/three-pages-platypus-times.pdf",
    "traditional-chinese/gov-letter-embedded-ttfsubset.pdf",
    "vector-heavy/dense-polyline-map-300k.pdf",
    "cad/a0-floorplan-layers.pdf",
    "scanned/scan-gray-jpeg-20p.pdf",
    "image-heavy/photos-rgb-jpeg-6p.pdf",
]
SCALES = [1.0, 2.0, 6.0]
TILES = [256, 512, 1024]
WORKERS = [1, 2, 4, 6, 8]


def run_once(bench: Path, fixture: str, scale: float, tile: int, workers: int) -> dict | None:
    cmd = [
        str(bench), "render", str(ROOT / "fixtures" / "generated" / fixture),
        "--scale", str(scale), "--tile", str(tile), "--workers", str(workers),
        "--viewport", "1920x1080",
    ]
    out = subprocess.run(cmd, capture_output=True, timeout=300)
    try:
        report = json.loads(out.stdout)
    except json.JSONDecodeError:
        return None
    if report.get("status") not in ("ok", "partial") or report.get("first_page_ms") is None:
        return None
    return {"ms": report["first_page_ms"], "rss_mb": report.get("rss_peak_mb"), "tiles": report.get("tiles")}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bench", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--out", type=Path, default=ROOT / "benchmarks" / "runs" / "b3b4.json")
    args = parser.parse_args()

    results = []
    combos = list(itertools.product(FIXTURES, SCALES, TILES, WORKERS))
    for i, (fixture, scale, tile, workers) in enumerate(combos, 1):
        runs = [r for r in (run_once(args.bench, fixture, scale, tile, workers) for _ in range(args.repeat)) if r]
        if not runs:
            print(f"[{i}/{len(combos)}] {fixture} x{scale} t{tile} w{workers}: failed", file=sys.stderr)
            continue
        row = {
            "fixture": fixture, "scale": scale, "tile": tile, "workers": workers,
            "ms": statistics.median(r["ms"] for r in runs),
            "rss_mb": statistics.median(r["rss_mb"] for r in runs if r["rss_mb"] is not None),
            "tiles": runs[0]["tiles"],
        }
        results.append(row)
        print(f"[{i}/{len(combos)}] {fixture} x{scale} t{tile} w{workers}: {row['ms']:.1f} ms", file=sys.stderr)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(results, indent=2), encoding="utf-8")

    # Summary: geometric mean of viewport-fill time per (tile, workers),
    # normalized per fixture/scale against the 512 x 4 configuration.
    def key(r: dict) -> tuple:
        return (r["fixture"], r["scale"])

    reference = {key(r): r["ms"] for r in results if r["tile"] == 512 and r["workers"] == 4}
    print("| tile \\ workers | " + " | ".join(str(w) for w in WORKERS) + " |")
    print("|---|" + "---|" * len(WORKERS))
    for tile in TILES:
        cells = []
        for workers in WORKERS:
            ratios = [r["ms"] / reference[key(r)] for r in results
                      if r["tile"] == tile and r["workers"] == workers and reference.get(key(r))]
            cells.append(f"{statistics.geometric_mean(ratios):.2f}" if ratios else "-")
        print(f"| {tile} | " + " | ".join(cells) + " |")
    print("\n(geometric mean of viewport-fill time relative to tile 512 / 4 workers; lower is better)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
