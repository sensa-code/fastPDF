#!/usr/bin/env python3
"""Paired B-1 comparison of two fastpdf-bench builds (spec §30).

`fastpdf-bench compare` checks a run against `benchmarks/baseline.json`,
which only works on a quiet machine: under background load every number
drifts and the 10% threshold flags noise. This tool cancels the drift
instead. For every corpus file it runs `fastpdf-bench full <file>` with
both builds in alternating order (A B, B A, ...), each run a fresh
process, and reports the median of the per-pair ratios (B / A).

Usage:
    python tools/bench_paired.py --a OLD/fastpdf-bench.exe --b NEW/fastpdf-bench.exe
        [--manifest fixtures/generated/manifest.json] [--pairs 3] [--timeout 120]
        [--out benchmarks/runs/paired.json]

Build each binary from a clean export of its commit (`git archive`) and copy
it out of the target dir before running, so a rebuild cannot swap it midway.

Reading the result:
- `cpu_ms` comes from the OS in 15.625 ms steps on Windows: ratios of short
  runs jump between 0.5x, 1x and 2x and mean nothing.
- Sub-millisecond metrics (open, text on small files) vary by 10-25% between
  runs; judge them by the absolute difference, not the ratio.
- A status change (ok -> partial, open_error -> crash, ...) is always a
  finding, whatever the timings say.
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
METRICS = ["open_ms", "first_page_ms", "time_to_first_page_ms", "text_ms",
           "rss_peak_mb", "private_peak_mb", "cpu_ms"]
# A change is judged only when the metric is above FLOOR and the medians
# differ by at least MIN_DELTA (ms / MB); below that, run-to-run jitter
# dominates the ratio.
FLOOR = {"open_ms": 1.0, "first_page_ms": 1.0, "time_to_first_page_ms": 1.0,
         "text_ms": 1.0, "rss_peak_mb": 5.0, "private_peak_mb": 5.0, "cpu_ms": 100.0}
MIN_DELTA = {"open_ms": 1.0, "first_page_ms": 1.0, "time_to_first_page_ms": 1.0,
             "text_ms": 1.0, "rss_peak_mb": 2.0, "private_peak_mb": 2.0, "cpu_ms": 31.25}


def run(exe: Path, pdf: Path, password: str | None, timeout: int) -> dict:
    cmd = [str(exe), "full", str(pdf), "--engine", "hayro"]
    if password:
        cmd += ["--password", password]
    try:
        out = subprocess.run(cmd, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"status": "timeout"}
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        return {"status": "crash", "exit_code": out.returncode}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--a", type=Path, required=True, help="baseline fastpdf-bench")
    parser.add_argument("--b", type=Path, required=True, help="candidate fastpdf-bench")
    parser.add_argument("--manifest", type=Path, default=ROOT / "fixtures" / "generated" / "manifest.json")
    parser.add_argument("--pairs", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--out", type=Path, default=ROOT / "benchmarks" / "runs" / "paired.json")
    args = parser.parse_args()
    a, b = args.a.resolve(), args.b.resolve()
    for exe in (a, b):
        if not exe.is_file():
            parser.error(f"{exe} does not exist")

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    entries = manifest["files"] if isinstance(manifest, dict) else manifest
    base = args.manifest.parent

    rows = []
    started = time.time()
    for n, entry in enumerate(entries, 1):
        pdf = base / entry["path"]
        runs: dict[str, list[dict]] = {"a": [], "b": []}
        for i in range(args.pairs):
            order = ("a", "b") if i % 2 == 0 else ("b", "a")
            for side in order:
                exe = a if side == "a" else b
                runs[side].append(run(exe, pdf, entry.get("password"), args.timeout))
        row = {"file": entry["path"], "category": entry.get("category"),
               "status_a": runs["a"][0].get("status"), "status_b": runs["b"][0].get("status")}
        for m in METRICS:
            av = [r.get(m) for r in runs["a"]]
            bv = [r.get(m) for r in runs["b"]]
            if all(isinstance(v, (int, float)) for v in av + bv):
                row[f"{m}_a"] = statistics.median(av)
                row[f"{m}_b"] = statistics.median(bv)
                ratios = [y / x for x, y in zip(av, bv) if x > 0]
                if ratios:
                    row[f"{m}_ratio"] = statistics.median(ratios)
        rows.append(row)
        ttfp = row.get("time_to_first_page_ms_ratio")
        print(f"[{n}/{len(entries)}] {row['file']:60s} {row['status_a']}/{row['status_b']}"
              f"  ttfp x{ttfp:.3f}" if ttfp else f"[{n}/{len(entries)}] {row['file']}", file=sys.stderr)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({"a": str(a), "b": str(b), "pairs": args.pairs, "rows": rows},
                                   indent=2), encoding="utf-8")

    # Summary.
    changed = [r for r in rows if r["status_a"] != r["status_b"]]
    statuses = defaultdict(int)
    for r in rows:
        statuses[r["status_b"]] += 1
    print(f"{len(rows)} files in {time.time() - started:.0f} s; statuses (B): {dict(statuses)}")
    print(f"status changes: {len(changed)}")
    for r in changed:
        print(f"  {r['file']}: {r['status_a']} -> {r['status_b']}")
    both_ok = [r for r in rows if r["status_a"] == "ok" and r["status_b"] == "ok"]
    print("\n| metric | files | geomean B/A | > +10% (judged) | < -10% (judged) |")
    print("|---|---|---|---|---|")
    flagged = []
    for m in METRICS:
        ratios = [r[f"{m}_ratio"] for r in both_ok if r.get(f"{m}_ratio", 0) > 0]
        if not ratios:
            continue
        judged = [r for r in both_ok if r.get(f"{m}_ratio", 0) > 0 and max(r[f"{m}_a"], r[f"{m}_b"]) >= FLOOR[m]]
        worse = [r for r in judged
                 if r[f"{m}_ratio"] > 1.10 and r[f"{m}_b"] - r[f"{m}_a"] >= MIN_DELTA[m]]
        better = [r for r in judged
                  if r[f"{m}_ratio"] < 0.90 and r[f"{m}_a"] - r[f"{m}_b"] >= MIN_DELTA[m]]
        flagged += [(m, r) for r in worse]
        print(f"| {m} | {len(ratios)} | {statistics.geometric_mean(ratios):.3f} | {len(worse)} | {len(better)} |")
    print("\nregressions (> +10% and above the floors):" if flagged else "\nno regression above the floors")
    for m, r in flagged:
        print(f"  {m}: {r['file']}  {r[f'{m}_a']:.2f} -> {r[f'{m}_b']:.2f}  x{r[f'{m}_ratio']:.2f}")
    return 1 if changed or flagged else 0


if __name__ == "__main__":
    sys.exit(main())
