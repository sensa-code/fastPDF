#!/usr/bin/env python3
"""Regenerate vendor/gpui_windows from the pinned zed checkout (ADR 0011).

FastPDF builds GPUI's Windows platform crate from a vendored copy so that
its local patches (vendor/gpui_windows-patches/) apply. This script makes
that copy reproducible:

1. Finds the git checkout cargo made of the zed rev pinned in the root
   Cargo.toml (~/.cargo/git/checkouts/zed-*/<short rev>, or --checkout) and
   checks that its HEAD is that rev.
2. Copies crates/gpui_windows: build.rs, the .hlsl shaders and every source
   file. LICENSE-APACHE is a symlink in zed's repository (a stub file in some
   checkouts); its target, the repository's full LICENSE-APACHE, is copied.
3. Writes a standalone Cargo.toml: zed's workspace inheritance resolved with
   the same versions and features, zed's own crates (gpui, collections,
   gpui_util, scheduler) taken from the same git URL and rev as FastPDF's GPUI
   so that cargo unifies them, and no lints (neither zed's nor FastPDF's).
4. Applies vendor/gpui_windows-patches/*.patch in file-name order
   (git apply -p3, line endings kept as LF).
5. Puts a modification notice (Apache-2.0 section 4(b)) on the first line of
   every file a patch changes, and on Cargo.toml.

vendor/gpui_windows/FASTPDF-PATCHES.md is written by hand; the script keeps it.

Usage:
    python tools/vendor_gpui_windows.py [--checkout DIR]           # rewrite vendor/gpui_windows
    python tools/vendor_gpui_windows.py [--checkout DIR] --check   # compare only; exit 1 on a difference
    python tools/vendor_gpui_windows.py --unit-tests               # run the vsync module's unit tests

--check is for GPUI upgrades and CI: it regenerates the tree in a temporary
directory and compares it byte for byte with vendor/gpui_windows. It also
checks that the build uses the vendored crate: the root Cargo.toml has the
[patch] entry and Cargo.lock has no other gpui_windows (cargo only warns about
an unused patch and then builds zed's crate without the patches).

--unit-tests compiles vendor/gpui_windows/src/vsync.rs on its own (a temporary
crate pinned by FastPDF's Cargo.lock, built offline) and runs its tests, which
cover the frame demand and park policy of patch 0003. The crate's own test
target needs gpui's test-support feature, whose dependencies FastPDF does not
build.
"""

from __future__ import annotations

import argparse
import filecmp
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor" / "gpui_windows"
PATCHES = ROOT / "vendor" / "gpui_windows-patches"
CRATE = "crates/gpui_windows"
# Kept in VENDOR across regenerations: written by hand, not generated.
HAND_WRITTEN = {"FASTPDF-PATCHES.md"}
# Text files are written with LF line endings (the repository's .gitattributes).
TEXT_SUFFIXES = {".rs", ".toml", ".hlsl", ".md", ""}
# zed's own crates gpui_windows depends on: taken from FastPDF's GPUI source.
ZED_CRATES = {"gpui", "collections", "gpui_util", "scheduler"}
# Key order of a dependency in the generated manifest.
DEP_KEYS = ["git", "rev", "package", "version", "default-features", "features", "optional"]
# Dependencies with more features than this get a table of their own.
INLINE_FEATURES = 4


def fail(message: str) -> None:
    sys.exit(f"error: {message}")


def zed_source() -> tuple[str, str]:
    """The git URL and rev of GPUI in the root Cargo.toml."""
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    gpui = manifest.get("workspace", {}).get("dependencies", {}).get("gpui", {})
    if not isinstance(gpui, dict) or "git" not in gpui or "rev" not in gpui:
        fail("the root Cargo.toml has no `gpui = { git = ..., rev = ... }` workspace dependency")
    return gpui["git"], gpui["rev"]


def find_checkout(rev: str, given: Path | None) -> Path:
    if given is not None:
        candidates = [given]
    else:
        cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
        candidates = sorted((cargo_home / "git" / "checkouts").glob(f"zed-*/{rev[:7]}"))
        if not candidates:
            fail(f"no cargo checkout of zed {rev[:7]} under {cargo_home / 'git' / 'checkouts'}; "
                 "build FastPDF once (cargo fetches it), or pass --checkout")
    for checkout in candidates:
        head = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"],
                              capture_output=True, text=True)
        if head.returncode == 0 and head.stdout.strip() == rev and (checkout / CRATE).is_dir():
            return checkout
    fail(f"none of {', '.join(map(str, candidates))} is a zed checkout at {rev}")
    raise AssertionError  # unreachable


def symlinks(checkout: Path) -> dict[str, str]:
    """Paths under CRATE that git records as symlinks (mode 120000), with their targets."""
    out = subprocess.run(["git", "-C", str(checkout), "ls-files", "-s", "--", CRATE],
                         capture_output=True, text=True, check=True).stdout
    links = {}
    for line in out.splitlines():
        meta, path = line.split("\t", 1)
        if meta.split()[0] == "120000":
            target = subprocess.run(["git", "-C", str(checkout), "cat-file", "-p", meta.split()[1]],
                                    capture_output=True, text=True, check=True).stdout
            links[path] = target.strip()
    return links


def text(data: bytes, path: Path) -> bytes:
    return data.replace(b"\r\n", b"\n") if path.suffix in TEXT_SUFFIXES else data


def copy_crate(checkout: Path, dest: Path) -> None:
    crate = checkout / CRATE
    links = symlinks(checkout)
    for src in sorted(p for p in crate.rglob("*") if p.is_file()):
        rel = src.relative_to(crate)
        if rel.as_posix() == "Cargo.toml":
            continue
        repo_path = f"{CRATE}/{rel.as_posix()}"
        if repo_path in links:
            src = (src.parent / links[repo_path]).resolve()
            if not src.is_file() or checkout.resolve() not in src.parents:
                fail(f"{repo_path} links to {links[repo_path]}, which is not a file of the checkout")
        out = dest / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(text(src.read_bytes(), rel))


# --- Cargo.toml ----------------------------------------------------------------


def resolve_dependency(name: str, spec, workspace_deps: dict, git: str, rev: str) -> dict | str:
    """A dependency with zed's workspace inheritance resolved; zed's crates
    from FastPDF's GPUI git source."""
    if isinstance(spec, str):
        return spec
    spec = dict(spec)
    if spec.pop("workspace", False):
        base = workspace_deps.get(name)
        if base is None:
            fail(f"`{name}` is not a zed workspace dependency")
        base = {"version": base} if isinstance(base, str) else dict(base)
        extra = spec.pop("features", [])
        features = list(base.get("features", []))
        features += [f for f in extra if f not in features]
        if features:
            base["features"] = features
        base.update(spec)  # optional = true and the like
        spec = base
    if "path" in spec:
        if name not in ZED_CRATES:
            fail(f"`{name}` is a zed path dependency that is not in ZED_CRATES")
        del spec["path"]
        spec = {"git": git, "rev": rev, **spec}
    unknown = set(spec) - set(DEP_KEYS)
    if unknown:
        fail(f"`{name}`: unexpected dependency keys {sorted(unknown)}")
    if set(spec) == {"version"}:
        return spec["version"]
    return {k: spec[k] for k in DEP_KEYS if k in spec}


def toml_value(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(v) for v in value) + "]"
    fail(f"unsupported manifest value {value!r}")
    raise AssertionError  # unreachable


def toml_key(key: str) -> str:
    if re.fullmatch(r"[A-Za-z0-9_-]+", key):
        return key
    # A literal string, as in zed's manifests: [target.'cfg(target_os = "windows")'...]
    return f"'{key}'" if "'" not in key else toml_value(key)


def dependency_line(name: str, spec: dict | str) -> str:
    if isinstance(spec, str):
        return f"{toml_key(name)} = {toml_value(spec)}"
    fields = ", ".join(f"{k} = {toml_value(v)}" for k, v in spec.items())
    return f"{toml_key(name)} = {{ {fields} }}"


def dependency_table(header: str, name: str, spec: dict) -> list[str]:
    lines = ["", f"[{header}.{toml_key(name)}]"]
    for key, value in spec.items():
        if key == "features":
            lines += ["features = ["] + [f"    {toml_value(f)}," for f in value] + ["]"]
        else:
            lines.append(f"{key} = {toml_value(value)}")
    return lines


def dependency_sections(header: str, deps: dict, workspace_deps: dict, git: str, rev: str) -> list[str]:
    inline, tables = [], []
    for name, spec in deps.items():
        resolved = resolve_dependency(name, spec, workspace_deps, git, rev)
        if isinstance(resolved, dict) and len(resolved.get("features", [])) > INLINE_FEATURES:
            tables += dependency_table(header, name, resolved)
        else:
            inline.append(dependency_line(name, resolved))
    lines = []
    if inline:
        lines += ["", f"[{header}]", *inline]
    return lines + tables


def manifest(checkout: Path, git: str, rev: str) -> str:
    crate = tomllib.loads((checkout / CRATE / "Cargo.toml").read_text(encoding="utf-8"))
    zed = tomllib.loads((checkout / "Cargo.toml").read_text(encoding="utf-8"))
    workspace_package = zed["workspace"]["package"]
    workspace_deps = zed["workspace"]["dependencies"]

    package = crate["package"]
    resolved_package = {}
    for key in ("name", "version", "edition", "publish", "license"):
        value = package.get(key)
        if isinstance(value, dict) and value.get("workspace"):
            value = workspace_package[key]
        if value is not None:
            resolved_package[key] = value
    lines = [
        "# Modified by FastPDF: standalone manifest without zed's workspace (zed's crates",
        "# from the same git URL and rev as FastPDF's GPUI); generated by",
        "# tools/vendor_gpui_windows.py. See FASTPDF-PATCHES.md.",
        "[package]",
        *(f"{k} = {toml_value(v)}" for k, v in resolved_package.items()),
        "",
        "# Upstream code kept as is: neither zed's nor FastPDF's lints apply.",
        "[lints]",
        "",
        "[lib]",
        *(f"{k} = {toml_value(v)}" for k, v in crate["lib"].items()),
        "",
        "[features]",
        *(f"{toml_key(k)} = {toml_value(v)}" for k, v in crate["features"].items()),
    ]
    if "dependencies" in crate:
        lines += dependency_sections("dependencies", crate["dependencies"], workspace_deps, git, rev)
    for target, tables in crate.get("target", {}).items():
        for kind in ("dependencies", "build-dependencies"):
            if kind in tables:
                header = f"target.{toml_key(target)}.{kind}"
                lines += dependency_sections(header, tables[kind], workspace_deps, git, rev)
    for name, table in crate.get("package", {}).get("metadata", {}).items():
        lines += ["", f"[package.metadata.{toml_key(name)}]",
                  *(f"{toml_key(k)} = {toml_value(v)}" for k, v in table.items())]
    unknown = set(crate) - {"package", "lints", "lib", "features", "dependencies", "target"}
    if unknown:
        fail(f"{CRATE}/Cargo.toml has sections this script does not handle: {sorted(unknown)}")
    return "\n".join(lines) + "\n"


# --- patches -------------------------------------------------------------------


def patch_files() -> list[Path]:
    patches = sorted(PATCHES.glob("*.patch"))
    if not patches:
        fail(f"no patches in {PATCHES}")
    return patches


def subject(patch: Path) -> str:
    """The patch's mbox Subject, without "[PATCH]" and the crate prefix."""
    head = patch.read_text(encoding="utf-8").split("\n---\n", 1)[0]
    match = re.search(r"^Subject: (.*(?:\n .*)*)", head, re.MULTILINE)
    if match is None:
        fail(f"{patch.name} has no Subject line")
    line = re.sub(r"\s*\n\s+", " ", match.group(1)).strip()
    return re.sub(r"^(\[PATCH[^\]]*\]\s*)?(gpui_windows:\s*)?", "", line)


def changed_files(patch: Path) -> list[str]:
    prefix = f"+++ b/{CRATE}/"
    return [line[len(prefix):] for line in patch.read_text(encoding="utf-8").splitlines()
            if line.startswith(prefix)]


def apply_patches(dest: Path) -> dict[str, list[str]]:
    """Applies the patches in order; returns {file: [patch notes]}."""
    inside = subprocess.run(["git", "rev-parse", "--is-inside-work-tree"],
                            cwd=dest, capture_output=True, text=True)
    if inside.stdout.strip() == "true":
        # git apply would resolve the patch's paths against that repository.
        fail(f"the staging directory {dest} is inside a git work tree; set TMP elsewhere")
    notes: dict[str, list[str]] = {}
    for patch in patch_files():
        result = subprocess.run(
            ["git", "-c", "core.autocrlf=false", "-c", "core.safecrlf=false",
             "apply", "-p3", "--whitespace=nowarn", str(patch)],
            cwd=dest, capture_output=True, text=True)
        if result.returncode != 0:
            fail(f"{patch.name} does not apply:\n{result.stderr.strip()}")
        number = patch.name.split("-", 1)[0]
        for path in changed_files(patch):
            notes.setdefault(path, []).append(f"{subject(patch)} ({number})")
    return notes


def add_notices(dest: Path, notes: dict[str, list[str]]) -> None:
    for path, items in sorted(notes.items()):
        file = dest / path
        comment = "#" if file.suffix == ".toml" else "//"
        notice = f"{comment} Modified by FastPDF: {'; '.join(items)}. See FASTPDF-PATCHES.md.\n"
        file.write_bytes(notice.encode("utf-8") + file.read_bytes())


# --- output --------------------------------------------------------------------


def generate(checkout: Path, git: str, rev: str, dest: Path) -> None:
    copy_crate(checkout, dest)
    (dest / "Cargo.toml").write_bytes(manifest(checkout, git, rev).encode("utf-8"))
    add_notices(dest, apply_patches(dest))


def tree(root: Path) -> list[str]:
    return sorted(p.relative_to(root).as_posix() for p in root.rglob("*")
                  if p.is_file() and p.relative_to(root).as_posix() not in HAND_WRITTEN)


def compare(expected: Path, actual: Path) -> list[str]:
    want, have = tree(expected), tree(actual)
    problems = [f"missing: {p}" for p in want if p not in have]
    problems += [f"not generated: {p}" for p in have if p not in want]
    problems += [f"differs: {p}" for p in want
                 if p in have and not filecmp.cmp(expected / p, actual / p, shallow=False)]
    return problems


def patch_in_use(git: str) -> list[str]:
    """Problems that keep the vendored crate out of the build."""
    problems = []
    root = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    entry = root.get("patch", {}).get(git, {}).get("gpui_windows")
    if not isinstance(entry, dict) or Path(entry.get("path", "")).as_posix() != "vendor/gpui_windows":
        problems.append(f'Cargo.toml: no [patch."{git}"] gpui_windows = {{ path = "vendor/gpui_windows" }}')
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    # A path package has no `source` in Cargo.lock; zed's crate would have its git source.
    sources = [p.get("source") for p in lock.get("package", []) if p.get("name") == "gpui_windows"]
    if sources != [None]:
        problems.append(f"Cargo.lock: gpui_windows sources {sources}, expected only the vendored "
                        "path crate (build once with --offline after changing the [patch])")
    return problems


def unit_tests(git: str, rev: str) -> int:
    """Runs the tests of vendor/gpui_windows/src/vsync.rs in a temporary crate."""
    vsync = VENDOR / "src" / "vsync.rs"
    zed = f'git = "{git}", rev = "{rev}"'
    crate_manifest = "\n".join([
        "[package]",
        'name = "gpui-windows-vsync-tests"',
        'version = "0.0.0"',
        'edition = "2024"',
        "publish = false",
        "",
        "[dependencies]",
        f"gpui = {{ {zed}, default-features = false }}",
        f"gpui_util = {{ {zed} }}",
        'anyhow = "1.0.86"',
        'log = "0.4.16"',
        'windows = { version = "0.62", features = ["Win32_Foundation", "Win32_Graphics_Dwm", '
        '"Win32_System_Performance"] }',
        "",
        "[workspace]",
        "",
    ])
    lib = (f"#[path = {toml_value(vsync.as_posix())}]\n"
           "#[allow(dead_code)]\n"
           "mod vsync;\n")
    target = os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))
    with tempfile.TemporaryDirectory(prefix="gpui_windows-vsync-") as tmp:
        crate = Path(tmp)
        (crate / "src").mkdir()
        (crate / "Cargo.toml").write_text(crate_manifest, encoding="utf-8")
        (crate / "src" / "lib.rs").write_text(lib, encoding="utf-8")
        shutil.copyfile(ROOT / "Cargo.lock", crate / "Cargo.lock")
        result = subprocess.run(
            ["cargo", "test", "--offline", "--target-dir", str(Path(target) / "vendor-unit-tests")],
            cwd=crate)
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--checkout", type=Path, help="zed git checkout at the pinned rev")
    parser.add_argument("--check", action="store_true",
                        help="compare the regenerated tree with vendor/gpui_windows; change nothing")
    parser.add_argument("--unit-tests", action="store_true",
                        help="run the unit tests of vendor/gpui_windows/src/vsync.rs")
    args = parser.parse_args()

    git, rev = zed_source()
    if args.unit_tests:
        return unit_tests(git, rev)
    checkout = find_checkout(rev, args.checkout)
    with tempfile.TemporaryDirectory(prefix="gpui_windows-") as tmp:
        staged = Path(tmp) / "gpui_windows"
        staged.mkdir()
        generate(checkout, git, rev, staged)
        if args.check:
            problems = compare(staged, VENDOR) if VENDOR.is_dir() else [f"missing: {VENDOR}"]
            problems += patch_in_use(git)
            for problem in problems:
                print(problem)
            result = f"{len(problems)} problem(s)" if problems else "matches, and the build uses it"
            print(f"vendor/gpui_windows vs zed {rev[:12]} + {len(patch_files())} patches: {result}")
            return 1 if problems else 0
        VENDOR.mkdir(parents=True, exist_ok=True)
        for old in sorted(VENDOR.rglob("*"), reverse=True):
            rel = old.relative_to(VENDOR).as_posix()
            if old.is_file() and rel not in HAND_WRITTEN:
                old.unlink()
            elif old.is_dir() and not any(old.iterdir()):
                old.rmdir()
        for rel in tree(staged):
            out = VENDOR / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(staged / rel, out)
    print(f"wrote {VENDOR.relative_to(ROOT)}: zed {rev[:12]} + {len(patch_files())} patches")
    return 0


if __name__ == "__main__":
    sys.exit(main())
