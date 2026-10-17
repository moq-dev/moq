"""Print the size table from `run.sh build`'s outputs (see README.md).

Bytes as built, stripped, and gzip -9. Stripping is `strip --strip-all` on a copy of a
native library, and dropping the `name` section of a `.wasm`, as the root `release`
profile's `strip = "symbols"` ships moq-wasm.
Generated bindings count every file a package ships for that language; JS glue is the
unminified ES modules a web build loads. Every `.wasm` is at opt-level z after
wasm-bindgen, without wasm-opt, as moq-wasm ships.
"""

import gzip
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

out = Path(sys.argv[1])


def files(*patterns: str) -> list[Path]:
    found = [path for pattern in patterns for path in sorted(out.glob(pattern)) if path.is_file()]
    if not found:
        raise SystemExit(f"size: nothing matches {patterns} under {out}; run `run.sh build` first")
    return found


def raw(paths: list[Path]) -> int:
    return sum(path.stat().st_size for path in paths)


def gz(paths: list[Path]) -> int:
    return sum(len(gzip.compress(path.read_bytes(), 9)) for path in paths)


def stripped(paths: list[Path]) -> int:
    total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for path in paths:
            copy = Path(tmp) / path.name
            shutil.copy(path, copy)
            subprocess.run(["strip", "--strip-all", str(copy)], check=True)
            total += copy.stat().st_size
    return total


def leb128(data: bytes, i: int) -> tuple[int, int]:
    value = shift = 0
    while True:
        byte = data[i]
        i += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if byte < 0x80:
            return value, i


def unnamed(path: Path) -> bytes:
    """The module without its `name` custom section, as `strip = "symbols"` ships it."""
    data = path.read_bytes()
    kept, i = [data[:8]], 8
    while i < len(data):
        start = i
        section = data[i]
        size, i = leb128(data, i + 1)
        body = data[i : i + size]
        i += size
        if section == 0:
            length, j = leb128(body, 0)
            if body[j : j + length] == b"name":
                continue
        kept.append(data[start:i])
    return b"".join(kept)


def kib(n: int) -> str:
    return f"{n / 1024:,.0f} KiB"


def table(title: str, header: list[str], rows: list[list[str]]) -> None:
    print(f"\n### {title}\n")
    print("| " + " | ".join(header) + " |")
    print("|" + "---|" * len(header))
    for row in rows:
        print("| " + " | ".join(row) + " |")


native = [
    ("slice, UniFFI", files("cargo/uniffi/release/libmoq_ffi_gen.so", "cargo/uniffi/release/libmoq_ffi_gen.dylib")),
    ("slice, BoltFFI", files("cargo/boltffi/release/libmoq_ffi_gen.so", "cargo/boltffi/release/libmoq_ffi_gen.dylib")),
    ("BoltFFI Python extension (`_native`)", files("boltffi/python-site/moq_ffi_gen/_native*")),
    ("BoltFFI Kotlin JNI glue", files("cargo/boltffi/release/libmoq_ffi_gen_jni.*")),
    ("moq-ffi, default features", files("cargo/moq-ffi/libmoq_ffi-default.*")),
    ("moq-ffi, no default features", files("cargo/moq-ffi/libmoq_ffi-no-default.*")),
]
table(
    "Native libraries",
    ["library", "as built", "stripped"],
    [[name, kib(raw(p)), kib(stripped(p))] for name, p in native],
)

bindings = [
    ("slice, UniFFI", "Python", files("uniffi/python/*.py")),
    ("slice, BoltFFI", "Python", files("boltffi/python-site/moq_ffi_gen/__init__.py*")),
    ("slice, UniFFI", "Kotlin", files("uniffi/kotlin/**/*.kt")),
    ("slice, BoltFFI", "Kotlin", files("boltffi/kotlin/**/*.kt")),
    ("slice, UniFFI", "Swift", files("uniffi/swift/*")),
    ("slice, BoltFFI", "Swift", files("boltffi/swift/*")),
    ("moq-ffi, UniFFI", "Python", files("moq-ffi/python/*.py")),
    ("moq-ffi, UniFFI", "Kotlin", files("moq-ffi/kotlin/**/*.kt")),
    ("moq-ffi, UniFFI", "Swift", files("moq-ffi/swift/*")),
]
table(
    "Generated bindings (source)",
    ["bindings", "language", "files", "raw", "gzip"],
    [[name, lang, str(len(p)), kib(raw(p)), kib(gz(p))] for name, lang, p in bindings],
)

runtime = files("node_modules/@boltffi/runtime/dist/*.js")
boltffi_glue = files(
    "boltffi/npm/web.js",
    "boltffi/npm/moq_ffi_gen.js",
    "boltffi/npm/moq_ffi_gen_bg.js",
    "boltffi/npm/moq_ffi_gen_imports.js",
)
wasm = [
    ("slice, UniFFI", files("uniffi/wasm/*.wasm"), None),
    ("slice, BoltFFI", files("boltffi/npm/*.wasm"), boltffi_glue + runtime),
    ("moq-ffi, UniFFI", files("moq-ffi/wasm/*.wasm"), None),
    ("moq-wasm, wasm-bindgen", files("moq-wasm/*.wasm"), files("moq-wasm/moq.js")),
]


def wasm_row(name: str, module: list[Path], js: list[Path] | None) -> list[str]:
    bare = [unnamed(path) for path in module]
    row = [name, kib(raw(module)), kib(sum(map(len, bare))), kib(sum(len(gzip.compress(b, 9)) for b in bare))]
    return row + ([kib(raw(js)), kib(gz(js))] if js else ["not generated"] * 2)


table(
    "Browser",
    ["module", ".wasm as built", ".wasm stripped", ".wasm stripped, gzip", "JS glue", "JS glue, gzip"],
    [wasm_row(name, w, js) for name, w, js in wasm],
)
