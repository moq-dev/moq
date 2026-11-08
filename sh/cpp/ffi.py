#!/usr/bin/env python3
"""Stage and fetch the default-feature native input used by C++ packages."""

import argparse
import gzip
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGETS = {
    "x86_64-unknown-linux-gnu": ("libmoq_ffi.a", "libmoq_ffi.so", "linux"),
    "aarch64-unknown-linux-gnu": ("libmoq_ffi.a", "libmoq_ffi.so", "linux"),
    "aarch64-apple-darwin": ("libmoq_ffi.a", "libmoq_ffi.dylib", "apple"),
    "x86_64-pc-windows-msvc": ("moq_ffi.lib", "moq_ffi.dll", "windows"),
}


def version(value):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", value):
        raise ValueError(f"unsupported FFI version: {value!r}")
    return value


def name(release, target):
    if target not in TARGETS:
        raise ValueError(f"unsupported native target: {target!r}")
    return f"moq-ffi-default-{version(release)}-{target}"


def stage(release, target, library, output):
    version(release)
    static, metadata, platform = TARGETS[target]
    for filename in (static, metadata):
        if not (library / filename).is_file():
            raise ValueError(f"missing native library: {library / filename}")
    output.mkdir(parents=True, exist_ok=False)
    (output / "lib").mkdir()
    for filename in (static, metadata):
        shutil.copyfile(library / filename, output / "lib" / filename)
    shutil.copyfile(ROOT / f"rs/moq-c/native-libs/{platform}.txt", output / "native-libs.txt")
    (output / "manifest.json").write_text(
        json.dumps(
            {
                "version": version(release),
                "target": target,
                "features": "default",
                "profile": "release",
                "source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            },
            indent=2,
        )
        + "\n"
    )


def archive(directory, output):
    manifest = json.loads((directory / "manifest.json").read_text())
    filename = name(manifest["version"], manifest["target"]) + ".tar.gz"
    output.mkdir(parents=True, exist_ok=True)
    path = output / filename
    # Deterministic headers let reruns refuse changed payloads without timestamp noise.
    with tempfile.TemporaryFile() as raw:
        with tarfile.open(fileobj=raw, mode="w") as tar:
            for path_in in sorted(directory.rglob("*")):
                info = tar.gettarinfo(str(path_in), str(path_in.relative_to(directory)))
                info.uid = info.gid = info.mtime = 0
                info.uname = info.gname = ""
                with path_in.open("rb") if path_in.is_file() else tempfile.TemporaryFile() as data:
                    tar.addfile(info, data if path_in.is_file() else None)
        raw.seek(0)
        with path.open("wb") as dest, gzip.GzipFile(filename="", mode="wb", fileobj=dest, mtime=0) as gz:
            shutil.copyfileobj(raw, gz)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    path.with_suffix(path.suffix + ".sha256").write_text(f"{digest}  {filename}\n")


def fetch(target, output):
    pins = [
        line.strip()
        for line in (ROOT / "cpp/moq/FFI_VERSION").read_text().splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]
    if len(pins) != 1:
        raise ValueError("FFI_VERSION must select one published version before fetching")
    release = version(pins[0])
    filename = name(release, target) + ".tar.gz"
    hashes = json.loads((ROOT / "cpp/moq/FFI_SHA256.json").read_text())
    for native_target in TARGETS:
        asset = name(release, native_target) + ".tar.gz"
        if not re.fullmatch(r"[a-f0-9]{64}", hashes.get(asset, "")):
            raise ValueError(f"FFI_SHA256.json lacks a SHA-256 pin for {asset}")
    expected = hashes[filename]
    if output.exists():
        raise ValueError(f"output already exists: {output}")
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp) / filename
        urllib.request.urlretrieve(
            f"https://github.com/moq-dev/moq/releases/download/moq-ffi-v{release}/{filename}", path
        )
        unpack(path, output, release, target, expected)


def unpack(path, output, release, target, expected):
    if output.exists():
        raise ValueError(f"output already exists: {output}")
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise ValueError(f"checksum mismatch: {path.name}")
    with tempfile.TemporaryDirectory() as tmp:
        extracted = pathlib.Path(tmp) / "extracted"
        # These archives contain only directories and regular files at their root.
        with tarfile.open(path) as tar:
            for member in tar.getmembers():
                rel = pathlib.PurePosixPath(member.name)
                if (
                    rel.is_absolute()
                    or ".." in rel.parts
                    or "\\" in member.name
                    or not (member.isfile() or member.isdir())
                ):
                    raise ValueError(f"unsupported archive member: {member.name}")
            extracted.mkdir()
            tar.extractall(extracted, filter="data")
        manifest = json.loads((extracted / "manifest.json").read_text())
        if (manifest["version"], manifest["target"], manifest["features"], manifest["profile"]) != (
            release,
            target,
            "default",
            "release",
        ):
            raise ValueError("downloaded native manifest disagrees with the selected release input")
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(extracted), output)


def extract(release, target, directory, output):
    filename = name(release, target) + ".tar.gz"
    sidecar = (directory / (filename + ".sha256")).read_text()
    if not re.fullmatch(r"[a-f0-9]{64}  " + re.escape(filename) + r"\n", sidecar):
        raise ValueError(f"malformed native checksum: {filename}")
    unpack(directory / filename, output, release, target, sidecar[:64])


def publish(release, directory, prev_tag):
    tag = f"moq-ffi-v{version(release)}"
    files = sorted(directory.iterdir())
    if len(files) != 8 or {p.name for p in files} != {
        name(release, target) + suffix for target in TARGETS for suffix in (".tar.gz", ".tar.gz.sha256")
    }:
        raise ValueError("publication requires all four native archives and checksums")
    for target in TARGETS:
        filename = name(release, target) + ".tar.gz"
        expected = hashlib.sha256((directory / filename).read_bytes()).hexdigest()
        if (directory / (filename + ".sha256")).read_text() != f"{expected}  {filename}\n":
            raise ValueError(f"native checksum disagrees with archive: {filename}")
    # Never clobber release inputs. Identical reruns are harmless; changed bytes fail.
    result = subprocess.run(["gh", "release", "view", tag], capture_output=True)
    if result.returncode:
        if b"release not found" not in result.stderr.lower():
            raise RuntimeError(result.stderr.decode())
        # Match the other moq-ffi publishers (sh/gh/release.sh) so the notes never depend on who wins the race.
        notes = ["--generate-notes"] + (["--notes-start-tag", prev_tag] if prev_tag else [])
        created = subprocess.run(
            ["gh", "release", "create", tag, "--verify-tag", "--title", f"moq-ffi v{release}", *notes]
        )
        if created.returncode:
            # Another language publisher can create the same release concurrently.
            subprocess.run(["gh", "release", "view", tag], check=True)
    assets = json.loads(subprocess.check_output(["gh", "release", "view", tag, "--json", "assets"], text=True))[
        "assets"
    ]
    existing = {asset["name"] for asset in assets}
    with tempfile.TemporaryDirectory() as tmp:
        for path in files:
            if path.name in existing:
                subprocess.run(["gh", "release", "download", tag, "--pattern", path.name, "--dir", tmp], check=True)
                if (pathlib.Path(tmp) / path.name).read_bytes() != path.read_bytes():
                    raise ValueError(f"refusing to replace published FFI asset: {path.name}")
        for path in files:
            if path.name not in existing:
                subprocess.run(["gh", "release", "upload", tag, str(path)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("stage", "fetch", "archive", "extract", "publish"):
        sub = commands.add_parser(command)
        if command in ("stage", "extract", "publish"):
            sub.add_argument("--version", required=True)
        if command in ("stage", "fetch", "extract"):
            sub.add_argument("--target", required=True, choices=TARGETS)
        if command == "stage":
            sub.add_argument("--library", type=pathlib.Path, required=True)
        if command in ("archive", "extract", "publish"):
            sub.add_argument("--directory", type=pathlib.Path, required=True)
        if command == "publish":
            sub.add_argument("--prev-tag", default="")
        if command != "publish":
            sub.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.command == "stage":
        stage(args.version, args.target, args.library, args.output)
    elif args.command == "archive":
        archive(args.directory, args.output)
    elif args.command == "fetch":
        fetch(args.target, args.output)
    elif args.command == "extract":
        extract(args.version, args.target, args.directory, args.output)
    else:
        publish(args.version, args.directory, args.prev_tag)


if __name__ == "__main__":
    main()
