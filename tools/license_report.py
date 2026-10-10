#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.md from `cargo metadata` (spec §36, §37).

Walks the resolved dependency graph of the workspace members (normal and
build dependencies; dev-dependencies are excluded because they never ship)
and lists every third-party crate with its license. Licenses outside the
permissive allowlist are flagged for manual review. Path dependencies outside
the workspace are third-party code vendored into the repository (e.g.
vendor/gpui_windows, built through [patch], ADR 0011): they are listed like
any other crate, with the folder they are vendored in.

--bundle DIR copies each crate's own license files into DIR/<crate>-<version>/
for the release zip (tools/package.ps1): MIT / BSD / ISC / Zlib need the
crate's copyright notice and license text, Apache-2.0 needs any NOTICE file.
  - Scope: the crates reachable from fastpdf-app through normal dependencies
    (same cargo metadata call, features and target as the report; proc-macro
    crates included). That covers everything the release binary links, plus a
    few extra crates: the metadata resolve also keeps weak optional
    dependencies (`dep?/feature`) that the build does not compile.
    Build-dependencies are left out: they only run at build time and are not
    linked into fastpdf.exe. Workspace crates are left out too.
  - Files: top-level LICENSE*, LICENCE*, COPYING*, NOTICE*, COPYRIGHT*,
    UNLICENSE* (any case) of the crate directory, copied byte for byte. Git
    dependencies also get the NOTICE* files of their repository root
    (in repository-root/).
  - Symlinks are followed inside the crate's source tree (the crate directory,
    or the whole checkout for git dependencies). A Windows checkout without
    symlink support stores a symlink as a small text file holding only the
    target, e.g. "../../LICENSE-APACHE"; such stubs are followed the same way
    and are never copied themselves.
  - Nothing is made up: crates without a license text and links that could not
    be followed are listed in DIR/MISSING.md. A crate without a license text
    whose license allows Apache-2.0 is covered by one shared DIR/Apache-2.0.txt
    (a copy of licenses/Apache-2.0.txt) and noted separately there.
  - Overrides: a crate without a license text of its own gets the files of
    licenses/overrides/<crate>-<version>/ (texts found in another version of
    the same crate or in another package of the same repository; see
    licenses/overrides/README.md). Only the exact version is matched, and only
    files listed in licenses/overrides/SOURCES.md with the same SHA-256 are
    used. Overrides that are stale or not used are reported in MISSING.md.
  - Output is deterministic (sorted, no timestamps). DIR must be empty or absent.
With --bundle the report is written only when --out is given.

--notices FILE writes the text fastpdf.exe embeds and prints with `fastpdf --licenses`
(THIRD_PARTY_NOTICES.txt at the repository root), so that a copy of the exe on its own
still carries the notices its dependencies' licenses require: the release also offers the
bare exe. It is --bundle's content for the same scope and options, in one file: FastPDF's
own LICENSE-MIT and LICENSE-APACHE, one line per crate with its license and its files,
then every distinct license file once (compared after normalizing the BOM, line endings,
trailing spaces and blank lines at either end). CI regenerates it and fails when the
committed file differs. Like --bundle, it writes the report only when --out is given.

Usage:
    python tools/license_report.py [--features "a,b"] [--all-features]
                                   [--target TRIPLE] [--out THIRD_PARTY_LICENSES.md] [--check]
    python tools/license_report.py --bundle DIR [--bundle-list FILE]
                                   [--features "a,b"] [--all-features] [--target TRIPLE]
    python tools/license_report.py --notices THIRD_PARTY_NOTICES.txt

--check exits with status 1 when a crate needs review, for CI.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# SPDX identifiers acceptable without review for a commercial, closed-or-open
# distribution (attribution obligations still apply and are listed below).
PERMISSIVE = {
    "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause", "BSD-3-Clause", "0BSD", "ISC", "Zlib", "BSL-1.0",
    "CC0-1.0", "Unlicense", "Unicode-3.0", "Unicode-DFS-2016", "bzip2-1.0.6",
}
# Need a human decision before shipping (copyleft or unusual terms).
REVIEW_HINTS = {
    "MPL-2.0": "weak copyleft (file-level): modified MPL files must stay MPL",
    "LGPL": "LGPL: static linking in Rust makes compliance hard; avoid",
    "GPL": "GPL: incompatible with closed-source distribution; avoid",
    "AGPL": "AGPL: avoid",
    "OFL-1.1": "font license: fine for bundled fonts, not for code",
    "OpenSSL": "OpenSSL/SSLeay terms: advertising clause",
}

# --bundle: the package whose binary ships (tools/package.ps1 builds `-p fastpdf-app`).
RELEASE_PACKAGE = "fastpdf-app"
# A crate's own license files, matched case-insensitively against the entries
# at the top of its directory.
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|copyright|unlicense)", re.IGNORECASE)
# The files among them that carry license terms (NOTICE and COPYRIGHT do not).
LICENSE_TEXT = re.compile(r"^(licen[cs]e|copying|unlicense)", re.IGNORECASE)
NOTICE_FILE = re.compile(r"^notice", re.IGNORECASE)
SHARED_APACHE = ROOT / "licenses" / "Apache-2.0.txt"
# License texts kept in the repository for crates that ship none (README.md there).
OVERRIDES = ROOT / "licenses" / "overrides"
# --notices: FastPDF's own license texts, first in the notices.
OWN_LICENSES = ("LICENSE-MIT", "LICENSE-APACHE")
OVERRIDE_FOLDER = re.compile(r"^(?P<name>[A-Za-z0-9_-]+?)-(?P<version>\d+\.\d+\.\d+\S*)$")


def cargo_metadata(features: str | None, all_features: bool, target: str) -> dict:
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target]
    if all_features:
        cmd.append("--all-features")
    elif features:
        cmd += ["--features", features]
    out = subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True)
    return json.loads(out.stdout)


def reachable(meta: dict, roots: list[str], follow) -> list[dict]:
    """Third-party packages reachable from `roots` through edges whose set of
    dependency kinds (None = normal, "build", "dev") passes `follow`."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    packages = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    seen: set[str] = set()
    stack = list(roots)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if follow({k.get("kind") for k in dep["dep_kinds"]}):
                stack.append(dep["pkg"])
    return sorted(
        (packages[p] for p in seen - members),
        key=lambda p: (p["name"], p["version"]),
    )


def shipped_packages(meta: dict) -> list[dict]:
    """Packages reachable from workspace members via non-dev edges."""
    return reachable(meta, list(meta["workspace_members"]), lambda kinds: bool(kinds - {"dev"}))


def linked_packages(meta: dict) -> list[dict]:
    """Packages reachable from RELEASE_PACKAGE through normal edges: what the
    release binary links (a superset, see the module docstring). Build-dependencies
    only run at build time."""
    members = set(meta["workspace_members"])
    roots = [p["id"] for p in meta["packages"] if p["name"] == RELEASE_PACKAGE and p["id"] in members]
    if not roots:
        sys.exit(f"error: {RELEASE_PACKAGE} is not a workspace member")
    return reachable(meta, roots, lambda kinds: None in kinds)


def classify(license_expr: str | None) -> tuple[bool, str]:
    """Returns (needs_review, note)."""
    if not license_expr:
        return True, "no license metadata"
    expr = license_expr.replace("/", " OR ")
    # An OR-expression is fine if any alternative is fully permissive.
    for alternative in re.split(r"\s+OR\s+", expr):
        terms = [t.strip("() ") for t in re.split(r"\s+AND\s+", alternative)]
        if all(t in PERMISSIVE for t in terms if t):
            return False, ""
    for key, hint in REVIEW_HINTS.items():
        if key in expr:
            return True, hint
    return True, "not in the permissive allowlist"


def ported_lines() -> list[str]:
    """Source files copied or adapted from other projects (tools/ported-sources.json)."""
    path = ROOT / "tools" / "ported-sources.json"
    entries = json.loads(path.read_text(encoding="utf-8")) if path.exists() else []
    if not entries:
        return ["None."]
    lines = [
        "| File | Source | Revision | License | Copyright | Changes |",
        "|---|---|---|---|---|---|",
    ]
    for e in entries:
        lines.append(
            f"| `{e['path']}` | {e['source']} | `{e['revision'][:12]}` | {e['license']} "
            f"| {e['copyright']} | {e['changes']} |"
        )
    lines += ["", "License texts: `licenses/`."]
    return lines


def repository(p: dict) -> str:
    """The crate's repository; a path dependency (only vendored third-party code
    is one: workspace members are never listed) also names its folder."""
    repo = p.get("repository") or ""
    if p.get("source") is not None:
        return repo
    folder = Path(p["manifest_path"]).parent
    try:
        where = f"vendored in `{folder.relative_to(ROOT).as_posix()}`"
    except ValueError:
        where = f"path dependency outside the repository: `{folder.as_posix()}`"
    return f"{where} ({repo})" if repo else where


def render(packages: list[dict], args: argparse.Namespace) -> tuple[str, int]:
    review = []
    rows = []
    for p in packages:
        needs, note = classify(p.get("license"))
        rows.append(f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | {repository(p)} |")
        if needs:
            review.append(f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | {note} |")
    feature_note = "all features" if args.all_features else (args.features or "default features")
    lines = [
        "# Third-Party Licenses",
        "",
        "<!-- Generated by tools/license_report.py — do not edit by hand. -->",
        "",
        f"Dependency graph: workspace members, {feature_note}, target `{args.target}`, "
        "normal + build dependencies "
        "(dev-dependencies excluded). Regenerate after every dependency change "
        "(spec §36–§37).",
        "",
        "## Needs review",
        "",
    ]
    if review:
        lines += ["| Crate | Version | License | Note |", "|---|---|---|---|", *review]
    else:
        lines.append("None — every shipped crate has a permissive license option.")
    lines += [
        "",
        "## Attribution obligations",
        "",
        "- Apache-2.0 crates: ship the license text and any `NOTICE` file contents with the binary.",
        "- MIT / BSD / ISC / Zlib crates: ship the copyright notice and license text.",
        "- Code copied or ported from upstream projects (e.g. pdf-reader-gpui, Apache-2.0) must keep",
        "  its copyright header and be listed in the section below.",
        "",
        "## Ported source",
        "",
        *ported_lines(),
        "",
        f"## All shipped crates ({len(packages)})",
        "",
        "| Crate | Version | License | Repository |",
        "|---|---|---|---|",
        *rows,
        "",
    ]
    return "\n".join(lines), len(review)


def link_target(path: Path) -> str | None:
    """Target of a symlink or of a symlink stub; None for an ordinary file.

    Git without symlink support (core.symlinks=false, common on Windows) checks
    a symlink out as a small text file holding only the target, e.g.
    "../../LICENSE-APACHE": one line, no whitespace, a path.
    """
    if path.is_symlink():
        return os.readlink(path)
    if path.stat().st_size > 1024:
        return None
    try:
        text = path.read_bytes().decode("utf-8").rstrip("\r\n")
    except UnicodeDecodeError:
        return None
    if not text or any(c.isspace() for c in text):
        return None
    if "/" in text or "\\" in text or LICENSE_FILE.match(text):
        return text
    return None


def within(path: Path, root: Path) -> bool:
    real, top = (os.path.normcase(os.path.realpath(p)) for p in (path, root))
    try:
        return os.path.commonpath([real, top]) == top
    except ValueError:  # another drive
        return False


def follow_links(path: Path, root: Path) -> tuple[Path | None, str | None]:
    """(file to copy, first link target). The file is None when a link leads out
    of `root`, to something that is not a file, or around in circles."""
    first = None
    for _ in range(8):
        target = link_target(path)
        if target is None:
            return path, first
        first = first or target
        path = Path(os.path.normpath(path.parent / target.replace("\\", "/")))
        if not within(path, root) or not path.is_file():
            return None, first
    return None, first


def source_root(pkg: dict, crate_dir: Path) -> Path:
    """The tree a crate's links may point into: the whole checkout for git
    dependencies (monorepos link each crate's LICENSE to the repository root),
    otherwise the crate directory. Cargo marks a finished checkout with .cargo-ok."""
    if (pkg.get("source") or "").startswith("git+"):
        for d in (crate_dir, *crate_dir.parents):
            if (d / ".cargo-ok").is_file():
                return d
    return crate_dir


def expand(name: str, path: Path):
    """A directory such as LICENSES/ (REUSE layout) stands for the files in it."""
    if path.is_dir() and not path.is_symlink():
        for sub in sorted(path.rglob("*")):
            if sub.is_symlink() or sub.is_file():
                yield f"{name}/{sub.relative_to(path).as_posix()}", sub
    else:
        yield name, path


def crate_license_files(pkg: dict) -> tuple[list[tuple[str, Path, bool]], list[tuple[str, str]]]:
    """([(name in the bundle, file to copy, reached through a link)],
    [(name in the bundle, link target that could not be followed)])."""
    crate_dir = Path(pkg["manifest_path"]).parent
    root = source_root(pkg, crate_dir)
    candidates = [(e, crate_dir / e) for e in sorted(os.listdir(crate_dir)) if LICENSE_FILE.match(e)]
    if root != crate_dir:  # git dependency: the repository's NOTICE covers its crates
        candidates += [(f"repository-root/{e}", root / e)
                       for e in sorted(os.listdir(root)) if NOTICE_FILE.match(e)]
    files, unresolved = [], []
    for name, path in (item for candidate in candidates for item in expand(*candidate)):
        real, link = follow_links(path, root)
        if real is None:
            unresolved.append((name, link or ""))
        else:
            files.append((name, real, link is not None))
    return files, unresolved


def allows_apache(license_expr: str | None) -> bool:
    """True when Apache-2.0 alone satisfies the SPDX expression (it is one of
    the OR alternatives). Expressions with parentheses are not simplified."""
    if not license_expr or "(" in license_expr:
        return False
    return any(alt.strip() == "Apache-2.0" for alt in re.split(r"\s+OR\s+|/", license_expr))


def label(p: dict) -> str:
    return f"{p['name']} {p['version']}"


def load_overrides(root: Path) -> tuple[dict[tuple[str, str], list[tuple[str, Path, str]]],
                                        list[tuple[str, str]]]:
    """License texts kept in the repository for crates that ship none:
    root/<crate>-<version>/<file>, each file listed in root/SOURCES.md as a table
    row "| `<crate>-<version>/<file>` | `<sha256>` | source |".

    Returns ({(crate, version): [(name, file, sha256)]}, [(folder, problem)]).
    A folder with an unlisted file or a SHA-256 mismatch is not used at all.
    """
    if not root.is_dir():
        return {}, []
    documented: dict[str, str] = {}
    sources = root / "SOURCES.md"
    if sources.is_file():
        for line in sources.read_text(encoding="utf-8").splitlines():
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if line.lstrip().startswith("|") and len(cells) >= 2:
                path = re.fullmatch(r"`([^`]+/[^`]+)`", cells[0])
                sha = re.fullmatch(r"`([0-9A-Fa-f]{64})`", cells[1])
                if path and sha:
                    documented[path.group(1)] = sha.group(1).lower()
    overrides, problems = {}, []
    for folder in sorted((d for d in root.iterdir() if d.is_dir()), key=lambda d: d.name):
        match = OVERRIDE_FOLDER.match(folder.name)
        files = sorted((f for f in folder.rglob("*") if f.is_file()), key=lambda f: f.as_posix())
        if not match:
            problems.append((folder.name, "folder name is not `<crate>-<version>`"))
            continue
        if not files:
            problems.append((folder.name, "no files"))
            continue
        entries, bad = [], []
        for f in files:
            rel = f.relative_to(root).as_posix()
            sha = hashlib.sha256(f.read_bytes()).hexdigest()
            if rel not in documented:
                bad.append(f"`{rel}` is not listed in SOURCES.md")
            elif documented[rel] != sha:
                bad.append(f"`{rel}` does not match its SHA-256 in SOURCES.md")
            entries.append((f.relative_to(folder).as_posix(), f, sha))
        if bad:
            problems.append((folder.name, "; ".join(bad)))
        else:
            overrides[(match["name"], match["version"])] = entries
    return overrides, problems


def render_missing(scope: str, without_text: list[dict], shared_apache: list[dict],
                   unresolved: list[tuple[dict, str, str]],
                   overridden: list[tuple[dict, list[tuple[str, Path, str]]]],
                   override_problems: list[tuple[str, str]]) -> str:
    def crates(packages: list[dict]) -> list[str]:
        if not packages:
            return ["None."]
        return ["| Crate | Version | License | Repository |", "|---|---|---|---|", *(
            f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | {repository(p) or '—'} |"
            for p in packages)]

    lines = [
        "# Missing third-party license texts",
        "",
        "<!-- Generated by tools/license_report.py --bundle — do not edit by hand. -->",
        "",
        f"- Scope: {scope}.",
        "- Build-dependencies are not included: they only run at build time and are not linked",
        "  into the binary.",
        "- Each `<crate>-<version>/` folder holds the crate's own `LICENSE*`, `LICENCE*`, `COPYING*`,",
        "  `NOTICE*`, `COPYRIGHT*` and `UNLICENSE*` files, copied byte for byte. For git dependencies,",
        "  `repository-root/` holds the `NOTICE*` files at the root of the repository.",
        "- A crate without a license file of its own gets the files of `licenses/overrides/<crate>-<version>/`",
        "  in the FastPDF repository, if that exact version is there; `licenses/overrides/SOURCES.md`",
        "  records where each file was found and its SHA-256.",
        "",
        f"## Crates without a license text ({len(without_text)})",
        "",
        "Their source package has no license file and `licenses/overrides/` has no text for this",
        "version. No copyright line is made up for them; `licenses/overrides/README.md` in the",
        "FastPDF repository explains how to add one before a public release.",
        "",
        *crates(without_text),
        "",
        f"## Filled in from licenses/overrides ({len(overridden)})",
        "",
        "These crates ship no license file. Their texts come from another version of the same crate,",
        "another package of the same repository, or the crate's upstream repository at a recorded",
        "commit (see `licenses/overrides/SOURCES.md`).",
        "",
    ]
    if overridden:
        lines += ["| Crate | Version | License | File | SHA-256 |", "|---|---|---|---|---|", *(
            f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | `{name}` | `{sha}` |"
            for p, files in overridden for name, _, sha in files)]
    else:
        lines.append("None.")
    lines += [
        "",
        f"## Overrides not used ({len(override_problems)})",
        "",
        "An override applies only to the crate version in its folder name and only when every file",
        "matches `licenses/overrides/SOURCES.md`. After an upgrade the crate is listed as missing",
        "until a text for the new version is added.",
        "",
    ]
    if override_problems:
        lines += ["| Override | Reason |", "|---|---|", *(
            f"| `{folder}` | {reason} |" for folder, reason in override_problems)]
    else:
        lines.append("None.")
    lines += [
        "",
        f"## Covered by the shared Apache-2.0 text ({len(shared_apache)})",
        "",
        "These crates have no license file either, but their license allows Apache-2.0, which",
        "needs no copyright line: `Apache-2.0.txt` in this folder applies to them.",
        "",
        *crates(shared_apache),
        "",
        f"## Links that could not be followed ({len(unresolved)})",
        "",
        "Symlinks, or symlink stubs (a text file holding only the link target), whose target is",
        "not a file inside the crate's source tree. They were not copied.",
        "",
    ]
    if unresolved:
        lines += ["| Crate | Version | File | Link target |", "|---|---|---|---|", *(
            f"| {p['name']} | {p['version']} | `{name}` | `{target}` |" for p, name, target in unresolved)]
    else:
        lines.append("None.")
    lines.append("")
    return "\n".join(lines)


def write_bundle(packages: list[dict], dest: Path, scope: str, index: dict | None = None) -> list[str]:
    """Copies each crate's license files to dest/<crate>-<version>/ and writes
    dest/MISSING.md. Returns the bundle's files, relative to dest, sorted.
    With `index`, also records there which package each folder holds ("folders",
    in bundle order) and which folders rely on the shared Apache-2.0 text
    ("shared_apache") or have no license text ("missing")."""
    if index is not None:
        index.update(folders={}, shared_apache=set(), missing=set())
    if dest.exists() and (not dest.is_dir() or any(dest.iterdir())):
        sys.exit(f"error: --bundle {dest} must be a new or empty directory")
    dest.mkdir(parents=True, exist_ok=True)
    overrides, override_problems = load_overrides(OVERRIDES)
    written: list[str] = []
    folders: set[str] = set()
    without_text, shared_apache, unresolved, notices, renamed, overridden = [], [], [], [], [], []
    followed = own_text = 0
    for p in sorted(packages, key=lambda p: (p["name"], p["version"], p["id"])):
        files, broken = crate_license_files(p)
        folder, n = f"{p['name']}-{p['version']}", 1
        while folder.lower() in folders:  # the same name and version from two sources
            n += 1
            folder = f"{p['name']}-{p['version']}-{n}"
        folders.add(folder.lower())
        if index is not None:
            index["folders"][folder] = p
        if n > 1:
            renamed.append(f"{folder} ({p.get('source')})")
        has_text = any(LICENSE_TEXT.match(name.split("/")[0]) for name, _, _ in files)
        own_text += has_text
        override = overrides.pop((p["name"], p["version"]), None)
        if override is not None:
            own = {name.lower() for name, _, _ in files}
            clash = [name for name, _, _ in override if name.lower() in own]
            if has_text:
                override_problems.append((folder, "not needed: the crate ships its own license text"))
            elif clash:
                override_problems.append((folder, f"same file name as the crate's own: {', '.join(clash)}"))
            else:
                files += [(name, src, False) for name, src, _ in override]
                overridden.append((p, override))
                has_text = any(LICENSE_TEXT.match(name.split("/")[0]) for name, _, _ in override)
        for name, src, via_link in files:
            out = dest / folder / name
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(src, out)
            written.append(f"{folder}/{name}")
            followed += via_link
        unresolved += [(p, name, target) for name, target in broken]
        if any(NOTICE_FILE.match(part) for name, _, _ in files for part in name.split("/")):
            notices.append(p)
        if not has_text:
            shared = allows_apache(p.get("license"))
            (shared_apache if shared else without_text).append(p)
            if index is not None:
                index["shared_apache" if shared else "missing"].add(folder)
    # Overrides left over: for another version (stale after an upgrade) or another graph.
    versions: dict[str, set[str]] = {}
    for p in packages:
        versions.setdefault(p["name"], set()).add(p["version"])
    for name, version in overrides:
        if name in versions:
            reason = (f"stale: the dependency graph has {name} {', '.join(sorted(versions[name]))}; "
                      "an override applies only to its exact version")
        else:
            reason = f"not used: no {name} in this dependency graph"
        override_problems.append((f"{name}-{version}", reason))
    override_problems.sort()
    if shared_apache:
        shutil.copyfile(SHARED_APACHE, dest / "Apache-2.0.txt")
        written.append("Apache-2.0.txt")
    missing_md = render_missing(scope, without_text, shared_apache, unresolved, overridden, override_problems)
    (dest / "MISSING.md").write_text(missing_md, encoding="utf-8", newline="\n")
    written.append("MISSING.md")

    print(f"wrote {dest} ({len(written)} files)")
    print(f"  crates: {len(packages)}, {own_text} with their own license text; files reached through a "
          f"symlink or symlink stub: {followed}")
    print(f"  licenses/overrides: {len(overridden)} crate(s) filled in "
          f"({', '.join(label(p) for p, _ in overridden) or 'none'}); not used: {len(override_problems)}")
    print(f"  no license text: {len(without_text)} missing, {len(shared_apache)} covered by "
          f"Apache-2.0.txt; links not followed: {len(unresolved)} (see MISSING.md)")
    print(f"  NOTICE ({len(notices)}): {', '.join(label(p) for p in notices) or 'none'}")
    if renamed:
        print(f"  same name and version twice: {', '.join(renamed)}")
    for folder, reason in override_problems:
        print(f"  WARNING: override {folder}: {reason}")
    # A link that could not be followed matters only when its crate ended up
    # without any license text; a crate covered by its own text, an override
    # or the shared Apache-2.0 text is complete.
    uncovered = [u for u in unresolved if any(u[0] is p for p in without_text)]
    if without_text or uncovered:
        print("  WARNING: license texts are incomplete; resolve MISSING.md before a public release")
    elif unresolved:
        print(f"  note: {len(unresolved)} link(s) not followed, all in crates whose license text is "
              "covered (see MISSING.md)")
    return sorted(written)


def notice_text(raw: bytes) -> str:
    """A license file as notice text: UTF-8 (undecodable bytes replaced), no BOM, LF line
    endings, no trailing spaces, no leading or trailing blank lines."""
    text = raw.decode("utf-8", errors="replace").lstrip("﻿")
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines) + "\n"


def render_notices(bundle: Path, index: dict, scope: str) -> str:
    """The text `fastpdf --licenses` prints (THIRD_PARTY_NOTICES.txt, embedded in the binary):
    FastPDF's own licenses, then every crate of the bundle with its license expression and the
    license files that came with it. Identical files (after notice_text) appear once."""
    texts: dict[str, int] = {}          # normalized text -> position in blocks
    blocks: list[list] = []             # [id, first file name, text, number of crates]

    def use(text: str, name: str) -> str:
        if text not in texts:
            texts[text] = len(blocks)
            blocks.append([f"T{len(blocks) + 1}", name, text, 0])
        block = blocks[texts[text]]
        block[3] += 1
        return block[0]

    wrap = lambda s: textwrap.wrap(s, width=100, break_on_hyphens=False)  # noqa: E731
    lines = [
        "FastPDF license notices",
        "=======================",
        "",
        "Generated by tools/license_report.py --notices; do not edit by hand.",
        "fastpdf.exe embeds this text and prints it with `fastpdf --licenses`.",
        "",
        "FastPDF",
        "-------",
        "",
        "Copyright (c) 2026 sensa-code and FastPDF contributors.",
        "Licensed under the MIT license or the Apache License, Version 2.0, at your option",
        "(MIT OR Apache-2.0); both texts are below as F1 and F2.",
        "",
        "Third-party code",
        "----------------",
        "",
        *wrap(f"fastpdf.exe contains the following crates ({scope}). Each line names a crate, "
              "its license and the license files that came with it, printed below under "
              "their T numbers."),
        "",
    ]
    shared = index["shared_apache"]
    for folder, p in index["folders"].items():
        refs = []
        crate_dir = bundle / folder
        files = sorted((f for f in crate_dir.rglob("*") if f.is_file()),
                       key=lambda f: f.relative_to(crate_dir).as_posix().lower()) if crate_dir.is_dir() else []
        for f in files:
            refs.append(f"{use(notice_text(f.read_bytes()), f.name)} {f.relative_to(crate_dir).as_posix()}")
        if folder in shared:
            refs.append(f"{use(notice_text(SHARED_APACHE.read_bytes()), 'Apache-2.0')} "
                        "(the crate ships no license text; Apache-2.0 is one of its options)")
        if folder in index["missing"]:
            refs.append("no license text found")
        lines += wrap(f"- {label(p)}, {p.get('license') or 'no license metadata'}: {'; '.join(refs)}")
    lines += ["", "Texts", "-----", ""]
    for n, name in enumerate(OWN_LICENSES, start=1):
        lines += [f"=== F{n}: FastPDF, {name} ===", "", notice_text((ROOT / name).read_bytes())]
    for tid, name, text, users in blocks:
        lines += [f"=== {tid}: {name}, {users} crate{'' if users == 1 else 's'} ===", "", text]
    return "\n".join(lines).rstrip("\n") + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--features")
    parser.add_argument("--all-features", action="store_true")
    parser.add_argument("--target", default="x86_64-pc-windows-msvc",
                        help="only count dependencies used on this target (Windows-first)")
    parser.add_argument("--out", type=Path,
                        help="report file (default THIRD_PARTY_LICENSES.md; with --bundle, none)")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--bundle", type=Path, metavar="DIR",
                        help=f"copy the license files of the crates linked into {RELEASE_PACKAGE} to DIR")
    parser.add_argument("--bundle-list", type=Path, metavar="FILE",
                        help="with --bundle: write the bundle's files (relative paths, one per line) to FILE")
    parser.add_argument("--notices", type=Path, metavar="FILE",
                        help=f"write the license notices embedded in {RELEASE_PACKAGE} "
                             "(THIRD_PARTY_NOTICES.txt, printed by `fastpdf --licenses`) to FILE")
    args = parser.parse_args()
    if args.bundle_list and not args.bundle:
        parser.error("--bundle-list needs --bundle")

    meta = cargo_metadata(args.features, args.all_features, args.target)
    review_count = 0
    if (args.bundle is None and args.notices is None) or args.out is not None:
        out = args.out or ROOT / "THIRD_PARTY_LICENSES.md"
        text, review_count = render(shipped_packages(meta), args)
        out.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {out} ({review_count} crate(s) need review)")
    elif args.check:
        _, review_count = render(shipped_packages(meta), args)
        print(f"{review_count} crate(s) need review")
    feature_note = "all features" if args.all_features else (args.features or "default features")
    scope = (f"crates reachable from `{RELEASE_PACKAGE}` through normal dependencies "
             f"(proc-macro crates included), {feature_note}, target `{args.target}`; workspace "
             "crates excluded. This covers every crate linked into the release binary; the "
             "`cargo metadata` resolve also keeps weak optional dependencies (`dep?/feature`), "
             "so a few listed crates may not be linked")
    if args.bundle is not None:
        files = write_bundle(linked_packages(meta), args.bundle, scope)
        if args.bundle_list:
            args.bundle_list.write_text("".join(f"{f}\n" for f in files), encoding="utf-8", newline="\n")
    if args.notices is not None:
        index: dict = {}
        with tempfile.TemporaryDirectory() as tmp:
            write_bundle(linked_packages(meta), Path(tmp) / "bundle", scope, index)
            text = render_notices(Path(tmp) / "bundle", index, scope)
        args.notices.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {args.notices} ({len(text.encode('utf-8')):,} bytes, "
              f"{len(index['folders'])} crates)")
    return 1 if args.check and review_count else 0


if __name__ == "__main__":
    sys.exit(main())
