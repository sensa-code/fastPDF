#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.md from `cargo metadata` (spec §36, §37).

Walks the resolved dependency graph of the workspace members (normal and
build dependencies; dev-dependencies are excluded because they never ship)
and lists every third-party crate with its license. Licenses outside the
permissive allowlist are flagged for manual review.

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

Usage:
    python tools/license_report.py [--features "a,b"] [--all-features]
                                   [--target TRIPLE] [--out THIRD_PARTY_LICENSES.md] [--check]
    python tools/license_report.py --bundle DIR [--bundle-list FILE]
                                   [--features "a,b"] [--all-features] [--target TRIPLE]

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


def render(packages: list[dict], args: argparse.Namespace) -> tuple[str, int]:
    review = []
    rows = []
    for p in packages:
        needs, note = classify(p.get("license"))
        repo = p.get("repository") or ""
        rows.append(f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | {repo} |")
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
            f"| {p['name']} | {p['version']} | {p.get('license') or '—'} | {p.get('repository') or '—'} |"
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
        "These crates ship no license file. Their texts were found in another version of the same",
        "crate or in another package of the same repository (see `licenses/overrides/SOURCES.md`).",
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


def write_bundle(packages: list[dict], dest: Path, scope: str) -> list[str]:
    """Copies each crate's license files to dest/<crate>-<version>/ and writes
    dest/MISSING.md. Returns the bundle's files, relative to dest, sorted."""
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
            (shared_apache if allows_apache(p.get("license")) else without_text).append(p)
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
    if without_text or unresolved:
        print("  WARNING: license texts are incomplete; resolve MISSING.md before a public release")
    return sorted(written)


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
    args = parser.parse_args()
    if args.bundle_list and not args.bundle:
        parser.error("--bundle-list needs --bundle")

    meta = cargo_metadata(args.features, args.all_features, args.target)
    review_count = 0
    if args.bundle is None or args.out is not None:
        out = args.out or ROOT / "THIRD_PARTY_LICENSES.md"
        text, review_count = render(shipped_packages(meta), args)
        out.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {out} ({review_count} crate(s) need review)")
    elif args.check:
        _, review_count = render(shipped_packages(meta), args)
        print(f"{review_count} crate(s) need review")
    if args.bundle is not None:
        feature_note = "all features" if args.all_features else (args.features or "default features")
        scope = (f"crates reachable from `{RELEASE_PACKAGE}` through normal dependencies "
                 f"(proc-macro crates included), {feature_note}, target `{args.target}`; workspace "
                 "crates excluded. This covers every crate linked into the release binary; the "
                 "`cargo metadata` resolve also keeps weak optional dependencies (`dep?/feature`), "
                 "so a few listed crates may not be linked")
        files = write_bundle(linked_packages(meta), args.bundle, scope)
        if args.bundle_list:
            args.bundle_list.write_text("".join(f"{f}\n" for f in files), encoding="utf-8", newline="\n")
    return 1 if args.check and review_count else 0


if __name__ == "__main__":
    sys.exit(main())
