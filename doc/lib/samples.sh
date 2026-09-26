#!/usr/bin/env bash
set -euo pipefail

# Print every fenced LANG sample in the given Markdown files as one source file,
# each sample wrapped in its own function, so a binding's check can compile the
# documentation against its wrapper. A fence whose info string carries `ignore`
# (```kotlin ignore) is skipped: build manifests, not code.
#
# Inputs a sample leaves undefined (`opus_init_bytes`, `pts`) come from a prelude
# the caller compiles alongside. Imports are hoisted above the functions, where
# Kotlin, Swift, C, and C++ require them.
#
# Usage: samples.sh python|kotlin|swift|c|cpp FILE...

lang="${1:?usage: samples.sh python|kotlin|swift|c|cpp FILE...}"
shift

awk -v lang="$lang" '
BEGIN {
	indent = ""; hoist = ""
	if (lang == "python") { opener = "async def _sample_%d() -> None:"; closer = "    pass\n"; indent = "    " }
	else if (lang == "kotlin") { hoist = "^import "; opener = "suspend fun docSample%d() {"; closer = "}\n" }
	else if (lang == "swift") { hoist = "^import "; opener = "func docSample%d() async throws {"; closer = "}\n" }
	else if (lang == "c") { hoist = "^#include "; opener = "static int doc_sample_%d(void) {"; closer = "    return 0;\n}\n" }
	else if (lang == "cpp") { hoist = "^#include "; opener = "[[maybe_unused]] static void doc_sample_%d() {"; closer = "}\n" }
	else { print "samples.sh: unknown language " lang > "/dev/stderr"; exit 2 }
	comment = (lang == "python") ? "#" : "//"
}
FNR == 1 { inside = 0 }
!inside && $0 ~ ("^```" lang "([ \t]|$)") {
	inside = 1
	skip = ($0 ~ /[ \t]ignore([ \t]|$)/)
	if (!skip) {
		n++
		body[++nbody] = sprintf("%s %s:%d", comment, FILENAME, FNR)
		body[++nbody] = sprintf(opener, n)
	}
	next
}
inside && /^```/ {
	inside = 0
	if (!skip) body[++nbody] = closer
	next
}
inside && !skip {
	if (hoist != "" && $0 ~ hoist) { if (!($0 in seen)) { seen[$0] = 1; imports[++nimports] = $0 } }
	else body[++nbody] = ($0 == "") ? "" : indent $0
}
END {
	if (n == 0) { print "samples.sh: no " lang " samples found" > "/dev/stderr"; exit 1 }
	for (i = 1; i <= nimports; i++) print imports[i]
	print ""
	for (i = 1; i <= nbody; i++) print body[i]
}
' "$@"
