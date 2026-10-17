#!/usr/bin/env python3
"""Fail when session code reaches a private model item that carries no marker.

The public model (`quest/m1/public-model/`) shrinks what the built-in sessions reach
past the public API to a marked set. Each item they reach is either pending, naming
the quest that makes it public, or deliberately private with its reason:

    // pending: /quest/m1/public-model/ingest.md
    // private: passthrough | tuning | capability gap | test | plumbing (JS only)

The marker sits in the comment and attribute lines directly above the item. A marker
anywhere else is stale and fails too, so a quest that publishes an item must delete
its mark, and a `pending` mark must name a quest file that still exists.

Rust: under `rs/moq-net/src/model/`, every restricted visibility needs a marker unless
it stays inside the model: `pub(self)`, `pub(super)` outside `mod.rs`, or
`pub(in crate::model...)`. The compiler then refuses any unmarked reach.

JS: nothing compiles reach, so this follows the import edges instead. A non-test file
under `js/net/src/{ietf,lite,connection}/` reaching a model module's name the package
entrypoint does not publish (an import, a namespace member, a `hooks.<name>` read, or
an `@internal` member call) needs a marker on that declaration, as does every member
of the `wire.ts` views. An import or read form it cannot follow, like a default import or
a destructured namespace, fails instead. Run from `just rs lint` and `just js check`.
"""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]

PRIVATE = {"passthrough", "tuning", "capability gap", "test", "plumbing"}
# Rust hosts no session plumbing beside the model, so `plumbing` is JS only.
RUST_PRIVATE = PRIVATE - {"plumbing"}
MARKER = re.compile(r"^\s*// (private|pending): (.*?)\s*$")

errors = []


def fail(path, line, message):
	errors.append(f"{path.relative_to(ROOT)}:{line + 1}: {message}")


def marker(path, index, line, private):
	"""Validate a marker line against the `private` reasons, returning whether it is one."""
	found = MARKER.match(line)
	if not found:
		return False
	kind, value = found.groups()
	if kind == "private" and value not in private:
		fail(path, index, f"unknown private marker {value!r}; use one of: {', '.join(sorted(private))}")
	if kind == "pending" and not (value.startswith("/quest/") and value.endswith(".md") and (ROOT / value[1:]).is_file()):
		fail(path, index, f"pending marker names {value!r}, which is not an existing quest file")
	return True


def block(lines, index, prefixes):
	"""The indices of the comment and attribute lines directly above `index`."""
	above = []
	while index > 0 and lines[index - 1].lstrip().startswith(prefixes):
		index -= 1
		above.append(index)
	return above


def markers(path, lines, private):
	"""Every marker line in a file, validated."""
	return {i for i, line in enumerate(lines) if marker(path, i, line, private)}


def require(path, lines, index, prefixes, what, used):
	"""Fail unless a marker sits directly above `index`; record the markers it consumes."""
	found = [i for i in block(lines, index, prefixes) if MARKER.match(lines[i])]
	used.update(found)
	if not found:
		fail(path, index, f"{what} needs a `// private: <reason>` or `// pending: <quest>` marker")


def stale(path, all_markers, used):
	for i in sorted(all_markers - used):
		fail(path, i, "marker is not on an item that needs one; delete it")


# Rust

RUST = ROOT / "rs/moq-net/src/model"
VISIBILITY = re.compile(r"\bpub\s*\(([^)]*)\)")
RUST_BLOCK = ("//", "#[", "#!")


def rust():
	for path in sorted(RUST.rglob("*.rs")):
		lines = path.read_text().split("\n")
		used = set()
		for i, line in enumerate(lines):
			if line.lstrip().startswith("//"):
				continue
			for scope in VISIBILITY.findall(line):
				scope = " ".join(scope.split())
				if scope == "self" or (scope == "super" and path != RUST / "mod.rs"):
					continue
				if re.fullmatch(r"in crate::model(::\w+)*", scope):
					continue
				require(path, lines, i, RUST_BLOCK, f"`pub({scope})` reaches past the model and", used)
		stale(path, markers(path, lines, RUST_PRIVATE), used)


# JS

SRC = ROOT / "js/net/src"
SESSIONS = ("ietf", "lite", "connection")
# Root modules that are wire or session plumbing rather than model; sessions reach them freely.
PLUMBING = {"auth_session.ts", "datagram_stream.ts", "error.ts", "index.ts", "mock.ts", "stream.ts"}
JS_BLOCK = ("//", "/*", "*", "@")

DECLARATION = r"^export\s+(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?:function\*?|class|interface|type|const|let|var|enum|namespace)\s+{}\b"
VALUE = r"^export\s+(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?:function\*?|class|const|let|var|enum|namespace)\s+{}\b"
REEXPORT = re.compile(r"export\s+(type\s+)?(\*(?:\s+as\s+\w+)?|\{[^}]*\})\s+from\s+\"(\.[^\"]+)\"", re.S)
IMPORT = re.compile(r"import\s+(type\s+)?(\{[^}]*\}|\*\s+as\s+\w+)\s+from\s+\"(\.[^\"]+)\"", re.S)
# Every relative import, so a form IMPORT does not parse (like a default import) fails instead of slipping by.
ANY_IMPORT = re.compile(r"\bimport\s+[^\"';]*?\s+from\s+\"(\.[^\"]+)\"", re.S)


def module(source, specifier):
	return (source.parent / specifier).resolve()


def names(spec):
	"""(name, local binding, type-only) for each entry of an `{ a, type b as c }` list."""
	for entry in spec.strip("{}").split(","):
		entry = " ".join(entry.split())
		if entry:
			kind = entry.startswith("type ")
			name, _, local = entry.removeprefix("type ").partition(" as ")
			yield name, local or name, kind


def internal(lines, index):
	"""Whether the JSDoc directly above `index` tags it `@internal`."""
	return any("@internal" in lines[i] for i in block(lines, index, JS_BLOCK))


def declaration(lines, name):
	pattern = re.compile(DECLARATION.format(re.escape(name)))
	for i, line in enumerate(lines):
		if pattern.match(line):
			return i
	return None


def published():
	"""What the package entrypoint publishes: whole modules, and (module, name) -> type-only."""
	whole, named = set(), {}
	pending = [SRC / "index.ts"]
	seen = set()
	while pending:
		source = pending.pop()
		if source in seen:
			continue
		seen.add(source)
		for type_only, spec, specifier in REEXPORT.findall(source.read_text()):
			target = module(source, specifier)
			if spec.startswith("*"):
				whole.add(target)
				pending.append(target)
				continue
			for name, _, kind in names(spec):
				key = (target, name)
				named[key] = named.get(key, True) and bool(type_only or kind)
	return whole, named


def js():
	whole, named = published()
	models = {
		path for path in SRC.glob("*.ts") if ".test" not in path.name and "test-util" not in path.name and path.name not in PLUMBING
	}
	files = {path: path.read_text().split("\n") for path in models}
	# The declarations sessions need marked, by file and line.
	needed = {path: {} for path in models}

	def public(path, name, type_only):
		lines = files[path]
		index = declaration(lines, name)
		if path in whole:
			# A name it re-exports from a package has no declaration here, and is public.
			return index is None or not internal(lines, index)
		# A name declared only as an interface or type alias has no value, so any reach of it is a type reach.
		type_only = type_only or not any(re.match(VALUE.format(re.escape(name)), line) for line in lines)
		kind = named.get((path, name))
		return kind is not None and (type_only or not kind)

	def reach(path, name, user):
		lines = files[path]
		index = declaration(lines, name)
		if index is None:
			fail(user, 0, f"cannot find the declaration of `{name}` in {path.relative_to(ROOT)}")
			return
		needed[path].setdefault(index, f"`{name}`, reached by session code,")

	# Members tagged `@internal` inside exported classes, which `stripInternal` hides.
	members = {}
	for path, lines in files.items():
		for i, line in enumerate(lines):
			found = re.match(r"^\t+(?:readonly\s+|static\s+|async\s+|get\s+)*(\w+)\s*[(<:?]", line)
			if found and internal(lines, i):
				members.setdefault(found.group(1), []).append((path, i))

	hooks_path = SRC / "internal.ts"
	hooks_lines = files[hooks_path]
	hooks_start = hooks_lines.index("export const hooks: {")
	hooks_end = hooks_lines.index("} = {", hooks_start)
	hooks = {}
	for i in range(hooks_start + 1, hooks_end):
		found = re.match(r"^\t(\w+)\??:", hooks_lines[i])
		if found:
			hooks[found.group(1)] = i

	def code(text):
		"""Session source without its comments and imports."""
		return IMPORT.sub("", re.sub(r"//[^\n]*|/\*.*?\*/", "", text, flags=re.S))

	def reads(text, binding):
		"""The members session code reads off `binding`, a local name or a dotted path."""
		return sorted(set(re.findall(rf"(?<![\w$.]){re.escape(binding)}\.(\w+)", code(text))))

	def reach_hooks(text, binding, user):
		# Only `binding.<name>` reads are followed, so refuse any other use, like destructuring.
		if re.search(rf"(?<![\w$.]){re.escape(binding)}(?![\w$])(?!\s*\.\s*\w)", code(text)):
			fail(user, 0, f"read `{binding}` only as `{binding}.<name>`, so the lint can follow each hook")
		for hook in reads(text, binding):
			if hook not in hooks:
				fail(user, 0, f"`hooks.{hook}` is not declared in internal.ts")
				continue
			needed[hooks_path].setdefault(hooks[hook], f"`hooks.{hook}`, reached by session code,")

	for directory in SESSIONS:
		for user in sorted((SRC / directory).rglob("*.ts")):
			if ".test" in user.name or "test-util" in user.name:
				continue
			text = user.read_text()
			for statement in ANY_IMPORT.finditer(text):
				if module(user, statement.group(1)) in models and not IMPORT.fullmatch(statement.group(0)):
					fail(user, 0, f"import a model module by name or namespace only: {' '.join(statement.group(0).split())}")
			for type_only, spec, specifier in IMPORT.findall(text):
				target = module(user, specifier)
				if target not in models:
					continue
				if spec.startswith("*"):
					alias = spec.split()[-1]
					if re.search(rf"\}}\s*=\s*{alias}\s*(?:;|$)", code(text), re.M):
						fail(user, 0, f"read `{alias}` only as `{alias}.<name>`, so the lint can follow each member")
					# A local variable may shadow the alias, so only the module's own declarations count.
					for name in reads(text, alias):
						if declaration(files[target], name) is not None and not public(target, name, bool(type_only)):
							reach(target, name, user)
					if target == hooks_path:
						reach_hooks(text, f"{alias}.hooks", user)
					continue
				for name, local, kind in names(spec):
					if not public(target, name, bool(type_only or kind)):
						reach(target, name, user)
					if target == hooks_path and name == "hooks":
						reach_hooks(text, local, user)
			for name, declared in members.items():
				if re.search(rf"\.{name}\b", text):
					for path, index in declared:
						needed[path].setdefault(index, f"`@internal` member `{name}`, reached by session code,")

	# Every member of a `wire.ts` view exists for the protocol layers.
	wire_path = SRC / "wire.ts"
	wire_lines = files[wire_path]
	views = re.search(r"^type View = (.*);$", "\n".join(wire_lines), re.M).group(1)
	for view in (name.strip() for name in views.split("|")):
		start = wire_lines.index(f"export interface {view} {{")
		for i in range(start + 1, wire_lines.index("}", start)):
			found = re.match(r"^\t(?:readonly\s+)?(\w+)\??\s*[(:<]", wire_lines[i])
			if found:
				needed[wire_path].setdefault(i, f"`{view}.{found.group(1)}` view member")

	for path in sorted(models):
		lines = files[path]
		used = set()
		for index, what in sorted(needed[path].items()):
			require(path, lines, index, JS_BLOCK, what, used)
		stale(path, markers(path, lines, PRIVATE), used)


rust()
js()
if errors:
	print("\n".join(errors), file=sys.stderr)
	print(f"model-markers: {len(errors)} problem(s); see sh/rs/model-markers.py", file=sys.stderr)
	sys.exit(1)
