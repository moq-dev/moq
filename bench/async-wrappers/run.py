#!/usr/bin/env python3
"""Materialize and compare wrapper generators without changing production crates."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import tarfile
import time

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
OUT = ROOT / ".tmp/async-wrappers"
VARIANTS = ("handwritten", "rules", "attribute")
MODULES = {
    "rs/kio/src/queue.rs": ("pop",),
    "rs/moq-net/src/model/broadcast.rs": ("requested_track", "closed", "wanted_changed"),
}


def command(args, cwd, log, env=None, success=True):
    start = time.monotonic()
    with log.open("w") as output:
        result = subprocess.run(args, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT)
    if (result.returncode == 0) != success:
        raise RuntimeError(f"unexpected exit {result.returncode}: {args}; see {log}")
    return round(time.monotonic() - start, 3)


def replace(path, old, new):
    text = path.read_text()
    if old not in text:
        raise RuntimeError(f"source moved: {old!r} in {path}")
    path.write_text(text.replace(old, new))


def prepare(variant, archive):
    tree = OUT / variant
    if tree.exists():
        raise RuntimeError(f"{tree} exists; choose a new --output directory")
    tree.mkdir()
    with tarfile.open(archive) as source:
        source.extractall(tree, filter="data")
    replace(tree / "rs/kio/Cargo.toml", "[features]\n", '[features]\nasync = []\n')
    replace(tree / "rs/moq-net/Cargo.toml", 'async = [', 'async = ["kio/async", ')
    replace(tree / "rs/kio/src/lib.rs", '#![warn(missing_docs)]',
            '#![warn(missing_docs)]\nextern crate self as kio;')
    if variant == "attribute":
        replace(tree / "rs/kio/Cargo.toml", 'async = []', 'async = ["macros"]')
        with (tree / "rs/kio/macros/src/lib.rs").open("a") as target:
            target.write("\n" + (HERE / "attribute.rs").read_text())
        replace(tree / "rs/kio/src/lib.rs", "pub use kio_macros::machine;",
                "pub use kio_macros::{machine, wrapper};")
    for module, names in MODULES.items():
        path = tree / module
        text = path.read_text()
        # Only top-level impl methods in these two known source files. Fail if
        # their spelling or inventory changes, rather than measuring a partial edit.
        pattern = r"(?:\t///[^\n]*\n)*(?:\t#\[[^\n]*\]\n)*\tpub async fn (" + "|".join(names) + r")\([^\n]*\)[^\n]*\{\n.*?\n\t}\n"
        expected = 1 if "kio/" in module else 4
        text, count = re.subn(pattern, "", text, flags=re.S)
        if count != expected:
            raise RuntimeError(f"expected {expected} wrappers, found {count}: {module}")
        seen = 0
        def lower(match):
            nonlocal seen
            seen += 1
            docs, attrs, name, receiver, waiter, waiter_ty, output, body = match.groups()
            async_name = name.removeprefix("poll_")
            if variant == "attribute":
                return f'{docs}{attrs}\t#[cfg_attr(feature = "async", kio::wrapper)]\n\tpub fn {name}({receiver}, {waiter}: &{waiter_ty}) -> Poll<{output}> {body}\n'
            if variant == "rules":
                return f'{docs}\t[{attrs.strip()}]\n\tpub fn {name} => {async_name}({receiver}, {waiter}: &{waiter_ty}) -> {output} {body}\n'
            return f'{docs}{attrs}\tpub fn {name}({receiver}, {waiter}: &{waiter_ty}) -> Poll<{output}> {body}\n{docs}\t#[cfg(feature = "async")]\n\tpub async fn {async_name}({receiver}) -> {output} {{\n\t\t::kio::wait(|waiter| self.{name}(waiter)).await\n\t}}\n'
        pattern = (r"((?:\t///[^\n]*\n)*)((?:\t#\[[^\n]*\]\n)*)\tpub fn "
                   r"(poll_(?:" + "|".join(names) + r"))\((&(?:mut )?self), (\w+): &([^\n]+?)\) -> Poll<([^\n]+)> (\{\n.*?\n\t})\n")
        if variant == "rules":
            # Wrap each declaration/body as a single macro input.
            def rules(match):
                return "\twrapper! {\n" + lower(match) + "\t}\n"
            text = re.sub(pattern, rules, text, flags=re.S)
            offset = re.match(r"(?://![^\n]*\n)*", text).end()
            text = text[:offset] + (HERE / "rules.rs").read_text() + "\n" + text[offset:]
        else:
            text = re.sub(pattern, lower, text, flags=re.S)
        if seen != expected:
            raise RuntimeError(f"expected {expected} polls, found {seen}: {module}")
        path.write_text(text)
    return tree


def environment(tree):
    env = os.environ.copy()
    # Every variant owns its cache. No remote compiler cache or enclosing wrapper
    # may turn a clean measurement into a cache hit.
    for key in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS"):
        env.pop(key, None)
    env.update(CARGO_TARGET_DIR=str(tree / "target"), CARGO_INCREMENTAL="1", CARGO_BUILD_JOBS="2")
    return env


def check(tree, package, label, features=True, success=True):
    args = ["cargo", "check", "--locked", "--offline", "--lib", "-p", package, "--no-default-features"]
    if features:
        args += ["--features", f"{package}/async"]
    return command(args, tree, OUT / f"{tree.name}-{package}-{label}.log", environment(tree), success)


def verify(tree):
    for package in ("kio", "moq-net"):
        check(tree, package, "on")
        check(tree, package, "off", False)
        args = ["cargo", "doc", "--locked", "--offline", "--no-deps", "--lib", "-p", package,
                "--no-default-features", "--features", f"{package}/async"]
        env = environment(tree)
        env["RUSTDOCFLAGS"] = "-D warnings"
        command(args, tree, OUT / f"{tree.name}-{package}-docs.log", env)
    for file, poll, wrapper, phrase in (
        ("kio/struct.Queue.html", "poll_pop", "pop", "Poll for the next item."),
        ("moq_net/broadcast/struct.Producer.html", "poll_requested_track", "requested_track", "Poll"),
    ):
        html = (tree / "target/doc" / file).read_text()
        # Compare actual rendered documentation, not just matching source attrs.
        docs = []
        for name in (poll, wrapper):
            section = html.split(f'id="method.{name}"', 1)[1].split('</details>', 1)[0]
            docs.append(section.split('<div class="docblock">', 1)[1])
        if docs[0] != docs[1] or phrase not in docs[0]:
            raise RuntimeError(f"docs differ: {file} {poll}/{wrapper}")
    # Malformed receiver: useful diagnostics for a mistake in the generator input.
    path = tree / "rs/kio/src/queue.rs"
    original = path.read_text()
    if tree.name == "attribute":
        bad = original.replace("pub fn poll_pop(&self,", "pub fn poll_pop(self,")
    elif tree.name == "rules":
        bad = original.replace("pub fn poll_pop => pop(&self,", "pub fn poll_pop => pop(self,")
    else:
        bad = original.replace("pub fn poll_pop(&self,", "pub fn poll_pop(self,")
    if bad == original:
        raise RuntimeError("diagnostic injection missed its target")
    path.write_text(bad)
    try:
        check(tree, "kio", "bad-receiver", success=False)
        diagnostic = (OUT / f"{tree.name}-kio-bad-receiver.log").read_text()
        expected = {"handwritten": "cannot move out", "rules": "no rules expected", "attribute": "wrapper requires a borrowed receiver"}[tree.name]
        if expected not in diagnostic:
            raise RuntimeError(f"missing expected diagnostic: {expected}")
    finally:
        path.write_text(original)
    print(f"verified {tree.name}: feature on/off, rustdoc, rejected owned receiver", flush=True)


def main():
    global OUT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=OUT)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    OUT = args.output.resolve()
    if not OUT.is_relative_to(ROOT / ".tmp"):
        parser.error("--output must be inside this checkout's .tmp/")
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    OUT.mkdir(parents=True, exist_ok=True)
    archive = OUT / "source.tar"
    with archive.open("wb") as output:
        subprocess.run(["git", "archive", "HEAD"], cwd=ROOT, stdout=output, check=True)
    trees = {variant: prepare(variant, archive) for variant in VARIANTS}
    results = {variant: {"kio_clean": [], "net_clean": [], "net_edit": []} for variant in VARIANTS}
    metadata = {
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "jobs": 2, "incremental": True, "rounds": args.rounds,
        "system": {key: getattr(os.uname(), key) for key in ("sysname", "release", "machine")},
    }
    if not args.verify_only:
        for round_number in range(args.rounds):
            order = VARIANTS[round_number % 3:] + VARIANTS[:round_number % 3]
            for variant in order:
                tree = trees[variant]
                if (tree / "target").exists():
                    shutil.rmtree(tree / "target")
                # kio separately exposes the dependency cost hidden by moq-net's
                # existing proc-macro graph. Then discard it for the net clean run.
                results[variant]["kio_clean"].append(check(tree, "kio", f"clean-{round_number}"))
                shutil.rmtree(tree / "target")
                results[variant]["net_clean"].append(check(tree, "moq-net", f"clean-{round_number}"))
                path = tree / "rs/moq-net/src/model/broadcast.rs"
                original = path.read_text()
                path.write_text(original + f"\n// edit round {round_number}\n")
                try:
                    results[variant]["net_edit"].append(check(tree, "moq-net", f"edit-{round_number}"))
                finally:
                    path.write_text(original)
                print(variant, round_number, results[variant], flush=True)
        (OUT / "results.json").write_text(json.dumps({"metadata": metadata, "seconds": results}, indent=2) + "\n")
        for variant, metrics in results.items():
            print(variant, {key: statistics.median(values) for key, values in metrics.items()}, flush=True)
    for tree in trees.values():
        verify(tree)


if __name__ == "__main__":
    main()
