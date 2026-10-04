#!/usr/bin/env python3
"""Enforce engine isolation (spec §4, §42 — M2 completion criterion).

Only the adapter crate of an engine may depend on that engine:

  * Cargo: no workspace crate except `fastpdf-engine-<name>` may list a
    dependency whose name starts with an engine prefix (hayro, zpdf, pdfium).
  * Source: no `.rs` file outside the adapter crate may mention the engine's
    crate path (`hayro::`, `hayro_syntax::`, `zpdf::`, ...).

Usage: python tools/check_engine_isolation.py   (exit status 1 on violation)
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ENGINES = {
    "hayro": "fastpdf-engine-hayro",
    "zpdf": "fastpdf-engine-zpdf",
    "pdfium": "fastpdf-engine-pdfium",
}


def engine_of(dep_name: str) -> str | None:
    for prefix in ENGINES:
        if dep_name == prefix or dep_name.startswith(prefix + "-") or dep_name.startswith(prefix + "_"):
            return prefix
    return None


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=ROOT, check=True, capture_output=True,
        ).stdout
    )
    violations: list[str] = []
    for pkg in meta["packages"]:
        for dep in pkg["dependencies"]:
            engine = engine_of(dep["name"])
            if engine and pkg["name"] != ENGINES[engine]:
                violations.append(f"{pkg['name']}: depends on `{dep['name']}` (only {ENGINES[engine]} may)")

        crate_dir = Path(pkg["manifest_path"]).parent
        own_engine = next((e for e, c in ENGINES.items() if c == pkg["name"]), None)
        pattern = re.compile(
            r"\b(" + "|".join(re.escape(e) for e in ENGINES if e != own_engine) + r")(_[a-z0-9_]+)?::"
        )
        for src in crate_dir.rglob("*.rs"):
            if "target" in src.parts:
                continue
            for lineno, line in enumerate(src.read_text(encoding="utf-8").splitlines(), 1):
                code = line.split("//", 1)[0]
                if pattern.search(code):
                    violations.append(f"{src.relative_to(ROOT)}:{lineno}: engine type used outside its adapter")

    if violations:
        print("engine isolation violated:")
        for v in violations:
            print("  " + v)
        return 1
    print("engine isolation OK: engine crates are only used by their adapters")
    return 0


if __name__ == "__main__":
    sys.exit(main())
