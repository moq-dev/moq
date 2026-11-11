#!/usr/bin/env python3
"""Fail when a `verify-*` skill's Coverage table names code that no longer exists.

Each row's Code cell is a repo-relative Rust path, then the symbols it covers,
each in backticks; another path starts that file's symbols. Parenthesized notes
are prose and go unchecked. A symbol `A::b` must define `A` and `b` in its file:
an item (`fn`, `struct`, `enum`, ...), a field, or a variant, and `b` must sit
inside `A`'s own definition or `impl` blocks, so a same-named member of another
type does not count. Lowercase leading segments name modules and are skipped.
A rename or deletion of modelled code fails here until the table, and so the
model's mapping, follows it.

`--root DIR` checks another tree, for the tests.
"""

import re
import sys
from pathlib import Path

TICKED = re.compile(r"`([^`]+)`")
PAREN = re.compile(r"\([^()]*\)")
ITEM = r"\b(?:fn|struct|enum|trait|type|mod|const|static|union)\s+{}\b"
# Where a type's members live: its definition, and each `impl` whose header `implements` names it.
DEFINITION = r"\b(?:struct|enum|union|trait)\s+{}\b[^;{{]*\{{"
IMPL = re.compile(r"\bimpl\b([^;{]*)\{")
# A field or variant: `name: T`, `Name,`, `Name(..)`, `Name {{`, or `Name` alone.
MEMBER = r"^\s*(?:pub(?:\([^)]*\))?\s+)?{}\s*(?::|,|\(|\{{|$)"


def rows(skill: Path):
    """Yield (line number, Code cell) for each body row of the Coverage table."""
    inside = False
    for number, line in enumerate(skill.read_text().splitlines(), 1):
        if line.startswith("## "):
            inside = line.strip() == "## Coverage"
            continue
        if not inside or not line.startswith("|"):
            continue
        cell = line.split("|")[1].strip()
        if cell == "Code" or set(cell) <= set("-: "):
            continue
        yield number, cell


def defines(source: str, name: str) -> bool:
    escaped = re.escape(name)
    return bool(re.search(ITEM.format(escaped), source) or re.search(MEMBER.format(escaped), source, re.MULTILINE))


def implements(header: str) -> str:
    """The last segment of the type an `impl` header implements, ignoring generics, bounds, and `where`."""
    flat, depth = "", 0
    for char in header:
        depth += {"<": 1, ">": -1}.get(char, 0)
        if depth == 0 and char != ">":
            flat += char
    flat = re.split(r"\bwhere\b", flat)[0]
    flat = re.split(r"\bfor\b", flat)[-1]
    path = re.findall(r"[A-Za-z_][\w:]*", flat)
    return path[0].split("::")[-1] if path else ""


def bodies(source: str, owner: str) -> str:
    """Join the brace-matched bodies of `owner`'s definition and `impl` blocks."""
    spans = []
    impls = (m for m in IMPL.finditer(source) if implements(m.group(1)) == owner)
    for match in sorted([*re.finditer(DEFINITION.format(re.escape(owner)), source), *impls], key=lambda m: m.start()):
        depth, start = 0, match.end() - 1
        for end in range(start, len(source)):
            depth += {"{": 1, "}": -1}.get(source[end], 0)
            if depth == 0:
                spans.append(source[start + 1 : end])
                break
    return "\n".join(spans)


def check(root: Path) -> list[str]:
    errors = []
    skills = sorted(root.glob(".claude/skills/verify-*/SKILL.md"))
    for skill in skills:
        where = skill.relative_to(root)
        found = False
        for number, cell in rows(skill):
            found = True
            tokens = TICKED.findall(PAREN.sub("", cell))
            source = None
            for token in tokens:
                if "/" in token:
                    path = root / token
                    if not token.endswith(".rs") or not path.is_file():
                        errors.append(f"{where}:{number}: `{token}` is not a Rust file in the tree")
                        source = ""
                    else:
                        source = path.read_text()
                    continue
                if source is None:
                    errors.append(f"{where}:{number}: the Code cell must start with a repo-relative path")
                    break
                if not source:
                    continue
                segments = token.split("::")
                for index, segment in enumerate(segments):
                    if index < len(segments) - 1 and not segment[:1].isupper():
                        continue
                    owner = segments[index - 1] if index and segments[index - 1][:1].isupper() else None
                    if not defines(bodies(source, owner) if owner else source, segment):
                        place = f"`{owner}`" if owner else "file"
                        errors.append(f"{where}:{number}: `{token}`: no `{segment}` defined in its {place}")
            if not tokens:
                errors.append(f"{where}:{number}: the Code cell names no code")
        if not found:
            errors.append(f"{where}: no Coverage table")
    return errors


def main() -> int:
    root = Path(sys.argv[2]) if sys.argv[1:2] == ["--root"] else Path(__file__).resolve().parent.parent
    errors = check(root)
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
