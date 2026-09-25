#!/usr/bin/env python3
"""Fail when a connection-stats field is missing from a binding's doc page.

`MoqConnectionStats` in `rs/moq-ffi/src/session.rs` is the source of truth:
every field it declares must appear, in the language's own casing, on each page
under `doc/lib/`, so a new field cannot land undocumented. Run from
`just js check` and `just rs check`.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# The C ABI pairs each value with a `*_valid` flag; the page has to say so.
C_PAGE = "doc/lib/c/index.md"


def camel(field: str) -> str:
	head, *rest = field.split("_")
	return head + "".join(word.capitalize() for word in rest)


def pascal(field: str) -> str:
	return "".join(word.capitalize() for word in field.split("_"))


# Page -> how a Rust field is spelled there. C and Python keep the snake_case
# of the generated record; Swift, Kotlin, and Dart lowerCamelCase it; Go
# PascalCases it.
PAGES = {
	"doc/lib/c/index.md": lambda field: field,
	"doc/lib/py/index.md": lambda field: field,
	"doc/lib/swift/index.md": camel,
	"doc/lib/kt/index.md": camel,
	"doc/lib/dart/index.md": camel,
	"doc/lib/go/index.md": pascal,
}


def fields() -> list[str]:
	"""The field names declared by `MoqConnectionStats`, in source order."""
	source = (ROOT / "rs/moq-ffi/src/session.rs").read_text()
	struct = source.split("pub struct MoqConnectionStats {", 1)[1].split("\n}", 1)[0]
	# Every `pub <name>: ...` line, whatever the type: a new field of any shape
	# must still be documented.
	found = re.findall(r"pub (\w+):", struct)
	if not found:
		sys.exit("stats-docs: no fields parsed from MoqConnectionStats")
	return found


def main() -> int:
	names = fields()
	missing = []
	for page, spell in PAGES.items():
		text = (ROOT / page).read_text()
		for field in names:
			name = spell(field)
			if not re.search(rf"\b{re.escape(name)}\b", text):
				missing.append(f"{page}: {name}")
	if "_valid" not in (ROOT / C_PAGE).read_text():
		missing.append(f"{C_PAGE}: no validity flags")

	if missing:
		print("stats-docs: connection stats are undocumented on:", file=sys.stderr)
		for line in missing:
			print(f"  {line}", file=sys.stderr)
		return 1
	return 0


if __name__ == "__main__":
	sys.exit(main())
