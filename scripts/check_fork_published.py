#!/usr/bin/env python3
"""The parser fork's published version must be the source in this tree.

arcform pins `arcform-sqlparser` by exact version, and release.yml publishes
the fork only when that version is not yet on crates.io. So an edit to
vendor/sqlparser-0.55.0 without a version bump would leave crates.io holding
the old source under the same number, and a published arcform would build
against it. This check refuses that.

WHAT IT COMPARES
    When the fork's version is on crates.io, it downloads that .crate (and
    checks it against the `cksum` the crates.io index lists for the version),
    runs `cargo package` on the fork here, and compares the two archives file
    by file: the same paths, each with the same SHA-256.

    It does not compare the two .crate files whole, because they differ on
    every commit even when no source changed. Three files in a .crate are
    written by cargo rather than taken from the source, so they are left out:

        .cargo_vcs_info.json  names the commit the crate was packaged from
        Cargo.toml            the manifest as cargo normalises it, which
                              varies with the cargo version
        Cargo.lock            resolved afresh against the registry

    The manifest as written is still compared, as `Cargo.toml.orig`.

    When the version is not on crates.io, there is nothing to compare, and the
    release will publish it.

        exit 0  the published version matches, or is not published yet
        exit 1  it is published and differs: bump the fork's patch version
        exit 2  the check could not run (network, cargo, a bad download)

USAGE
    scripts/check_fork_published.py [--manifest-path vendor/sqlparser-0.55.0/Cargo.toml]
    scripts/check_fork_published.py --self-test
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

DEFAULT_MANIFEST = "vendor/sqlparser-0.55.0/Cargo.toml"
INDEX = "https://index.crates.io"
DOWNLOAD = "https://static.crates.io/crates"
USER_AGENT = "arcform-release (+https://github.com/meridian-online/arcform)"
CARGO_WRITTEN = {".cargo_vcs_info.json", "Cargo.toml", "Cargo.lock"}


class CannotRun(Exception):
    pass


def index_path(name: str) -> str:
    """The sparse-index path for a crate name (cargo's rule, by name length)."""
    name = name.lower()
    if len(name) == 1:
        return f"1/{name}"
    if len(name) == 2:
        return f"2/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[0:2]}/{name[2:4]}/{name}"


def checksum_for(index_text: str, version: str) -> str | None:
    """The `cksum` of the index line whose `vers` is `version`, or None."""
    for line in index_text.splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        if entry.get("vers") == version:
            return entry["cksum"]
    return None


def _get(url: str) -> bytes | None:
    """The body at `url`, or None for a 404."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read()
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise CannotRun(f"{url}: HTTP {exc.code}") from None
    except (urllib.error.URLError, TimeoutError) as exc:
        raise CannotRun(f"{url}: {exc}") from None


def crate_files(data: bytes) -> dict[str, str]:
    """Each file in a .crate, by its path inside the crate, to its SHA-256.

    The files cargo writes itself (CARGO_WRITTEN) are left out.
    """
    out: dict[str, str] = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            path = member.name.split("/", 1)[1] if "/" in member.name else member.name
            if path in CARGO_WRITTEN:
                continue
            fh = tar.extractfile(member)
            if fh is None:
                continue
            out[path] = hashlib.sha256(fh.read()).hexdigest()
    return out


def differences(published: dict[str, str], local: dict[str, str]) -> list[str]:
    found = []
    for path in sorted(set(published) | set(local)):
        if path not in local:
            found.append(f"only in the published crate: {path}")
        elif path not in published:
            found.append(f"only in this tree: {path}")
        elif published[path] != local[path]:
            found.append(f"changed: {path}")
    return found


def package_locally(manifest: Path, name: str, version: str) -> bytes:
    with tempfile.TemporaryDirectory() as target:
        proc = subprocess.run(
            ["cargo", "package", "--manifest-path", str(manifest), "--no-verify", "--allow-dirty", "--target-dir", target],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise CannotRun(f"cargo package failed:\n{proc.stderr}")
        crate = Path(target) / "package" / f"{name}-{version}.crate"
        if not crate.is_file():
            raise CannotRun(f"cargo package wrote no {crate.name}")
        return crate.read_bytes()


def check(manifest: Path) -> int:
    try:
        import tomllib
    except ModuleNotFoundError:
        raise CannotRun("this check needs Python 3.11 or later (tomllib)") from None
    try:
        package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
        name, version = package["name"], package["version"]
    except (OSError, KeyError, tomllib.TOMLDecodeError) as exc:
        raise CannotRun(f"{manifest}: cannot read [package] name and version: {exc}") from None

    index = _get(f"{INDEX}/{index_path(name)}")
    cksum = checksum_for(index.decode("utf-8"), version) if index is not None else None
    if cksum is None:
        print(f"{name} {version} is not on crates.io yet; the release will publish it.")
        return 0

    published = _get(f"{DOWNLOAD}/{name}/{name}-{version}.crate")
    if published is None:
        raise CannotRun(f"the index lists {name} {version} but its .crate is not downloadable")
    if hashlib.sha256(published).hexdigest() != cksum:
        raise CannotRun(f"the downloaded {name}-{version}.crate does not match the index's checksum {cksum}")

    found = differences(crate_files(published), crate_files(package_locally(manifest, name, version)))
    if found:
        print(
            f"error: {name} {version} is already on crates.io, and its source differs from "
            f"{manifest.parent}:\n  " + "\n  ".join(found) + "\n"
            f"Bump the fork's patch version, in {manifest} and in arcform's exact pin, so the "
            "release publishes the new source.",
            file=sys.stderr,
        )
        return 1
    print(f"{name} {version} on crates.io matches {manifest.parent}, file for file.")
    return 0


# ── self-test ───────────────────────────────────────────────────────────────


def _crate(files: dict[str, bytes], top: str = "arcform-sqlparser-0.55.0") -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for path, body in files.items():
            info = tarfile.TarInfo(f"{top}/{path}")
            info.size = len(body)
            tar.addfile(info, io.BytesIO(body))
    return buf.getvalue()


def self_test() -> int:
    passed = 0
    failed: list[str] = []

    def equal(label: str, want, got) -> None:
        nonlocal passed
        if want == got:
            passed += 1
        else:
            failed.append(f"{label}: wanted {want!r}, got {got!r}")

    equal("index path, 17 characters", "ar/cf/arcform-sqlparser", index_path("arcform-sqlparser"))
    equal("index path, one character", "1/a", index_path("a"))
    equal("index path, two characters", "2/ab", index_path("ab"))
    equal("index path, three characters", "3/a/abc", index_path("abc"))
    equal("index path, four characters", "se/rd/serde", index_path("serde"))
    equal("index path, capitals", "se/rd/serde", index_path("Serde"))

    index = "\n".join(
        json.dumps({"name": "arcform-sqlparser", "vers": v, "cksum": c, "deps": [], "features": {}})
        for v, c in (("0.55.0", "aaa"), ("0.55.1", "bbb"))
    )
    equal("cksum for a listed version", "bbb", checksum_for(index, "0.55.1"))
    equal("cksum for an unlisted version", None, checksum_for(index, "0.55.2"))

    base = {
        "src/lib.rs": b"pub fn parse() {}\n",
        "Cargo.toml.orig": b'[package]\nname = "arcform-sqlparser"\nversion = "0.55.0"\n',
        "README.md": b"fork\n",
        "Cargo.toml": b"# normalised by cargo 1.94\n",
        "Cargo.lock": b"# lock A\n",
        ".cargo_vcs_info.json": b'{"git": {"sha1": "aaaa"}}\n',
    }

    def compare(edit: dict[str, bytes | None]) -> list[str]:
        local = dict(base)
        for path, body in edit.items():
            if body is None:
                local.pop(path, None)
            else:
                local[path] = body
        return differences(crate_files(_crate(base)), crate_files(_crate(local)))

    equal("the same source", [], compare({}))
    equal(
        "only the files cargo writes differ",
        [],
        compare({".cargo_vcs_info.json": b'{"git": {"sha1": "bbbb"}}\n', "Cargo.toml": b"# cargo 1.95\n", "Cargo.lock": b"# lock B\n"}),
    )
    equal("a source file edited", ["changed: src/lib.rs"], compare({"src/lib.rs": b"pub fn parse() { todo!() }\n"}))
    equal("the manifest as written edited", ["changed: Cargo.toml.orig"], compare({"Cargo.toml.orig": b"[package]\nedition = \"2024\"\n"}))
    equal("a source file added", ["only in this tree: src/pivot.rs"], compare({"src/pivot.rs": b"mod pivot;\n"}))
    equal("a source file removed", ["only in the published crate: README.md"], compare({"README.md": None}))

    print(f"self-test: {passed} passed, {len(failed)} failed")
    for f in failed:
        print(f"  FAILED {f}", file=sys.stderr)
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    parser.add_argument("--manifest-path", type=Path, default=Path(DEFAULT_MANIFEST))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    try:
        return check(args.manifest_path)
    except CannotRun as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
