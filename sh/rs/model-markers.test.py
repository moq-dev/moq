"""The model marker lint passes a marked tree and fails each kind of unmarked or stale reach."""

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parent / "model-markers.py"

TREE = {
	"quest/m1/open.md": "# [S] Open\n",
	"rs/moq-net/src/model/mod.rs": "mod group;\n// pending: /quest/m1/open.md\npub(crate) use group::Open;\n",
	"rs/moq-net/src/model/group.rs": (
		"pub(super) struct Inner;\n"
		"pub(in crate::model) fn helper() {}\n"
		"/// Reached by a session test.\n"
		"#[cfg(test)]\n"
		"// private: test\n"
		"pub(crate) struct Open;\n"
	),
	"js/net/src/index.ts": 'export * as Track from "./track.ts";\n',
	"js/net/src/track.ts": (
		"export class Subscriber {\n"
		"\t// pending: /quest/m1/open.md\n"
		"\t/** @internal */\n"
		"\tfork(): void {}\n"
		"}\n"
	),
	"js/net/src/internal.ts": (
		"// private: plumbing\n"
		"export const hooks: {\n"
		"\t// private: plumbing\n"
		"\tused: () => void;\n"
		"\tunused: () => void;\n"
		"} = {\n"
		"\tused: () => {},\n"
		"\tunused: () => {},\n"
		"};\n"
	),
	"js/net/src/wire.ts": (
		"export interface Broadcast {\n"
		"\t// private: plumbing\n"
		"\tsubscribe(): void;\n"
		"}\n"
		"type View = Broadcast;\n"
		"// private: plumbing\n"
		"export function wireOf(handle: object): View {\n"
		"\treturn handle as View;\n"
		"}\n"
	),
	"js/net/src/lite/session.ts": (
		'import { hooks } from "../internal.ts";\n'
		'import { wireOf } from "../wire.ts";\n'
		"hooks.used();\n"
		"wireOf({}).subscribe();\n"
		"subscriber.fork();\n"
	),
	"js/net/src/lite/session.test.ts": 'import { hooks } from "../internal.ts";\nhooks.unused();\n',
}


def run(**changes):
	"""Run the lint over TREE with `changes` ({path: (old, new)}) applied; return its stderr, or None on success."""
	with tempfile.TemporaryDirectory() as directory:
		root = Path(directory)
		for path, text in TREE.items():
			if path in changes:
				old, new = changes[path]
				assert old in text, (path, old)
				text = text.replace(old, new)
			(root / path).parent.mkdir(parents=True, exist_ok=True)
			(root / path).write_text(text)
		(root / "sh/rs").mkdir(parents=True)
		shutil.copy(SCRIPT, root / "sh/rs/model-markers.py")
		result = subprocess.run([sys.executable, root / "sh/rs/model-markers.py"], capture_output=True, text=True)
		return None if result.returncode == 0 else result.stderr


class TestModelMarkers(unittest.TestCase):
	def test_marked_tree_passes(self):
		self.assertIsNone(run())

	def test_unmarked_crate_visibility_fails(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", "pub(crate) struct Inner")})
		self.assertIn("group.rs:1: `pub(crate)`", error)

	def test_super_super_reaches_the_crate(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", "pub(in super::super) struct Inner")})
		self.assertIn("group.rs:1: `pub(in super::super)`", error)

	def test_super_in_mod_rs_reaches_the_crate(self):
		error = run(**{"rs/moq-net/src/model/mod.rs": ("mod group;", "pub(super) mod group;")})
		self.assertIn("mod.rs:1: `pub(super)`", error)

	def test_pending_on_a_missing_quest_fails(self):
		error = run(**{"rs/moq-net/src/model/mod.rs": ("/quest/m1/open.md", "/quest/m1/done.md")})
		self.assertIn("not an existing quest file", error)

	def test_unknown_private_reason_fails(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("private: test", "private: later")})
		self.assertIn("unknown private marker", error)

	def test_stale_rust_marker_fails(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", "// private: test\npub(super) struct Inner")})
		self.assertIn("group.rs:1: marker is not on an item", error)

	def test_session_reaching_an_unmarked_hook_fails(self):
		error = run(**{"js/net/src/lite/session.ts": ("hooks.used();", "hooks.used();\nhooks.unused();")})
		self.assertIn("internal.ts:5: `hooks.unused`", error)

	def test_unmarked_internal_member_fails(self):
		error = run(**{"js/net/src/track.ts": ("\t// pending: /quest/m1/open.md\n", "")})
		self.assertIn("track.ts:3: `@internal` member `fork`", error)

	def test_unmarked_unpublished_import_fails(self):
		error = run(**{"js/net/src/wire.ts": ("// private: plumbing\nexport function", "export function")})
		self.assertIn("wire.ts:6: `wireOf`", error)

	def test_unmarked_view_member_fails(self):
		error = run(**{"js/net/src/wire.ts": ("\t// private: plumbing\n", "")})
		self.assertIn("wire.ts:2: `Broadcast.subscribe` view member", error)

	def test_stale_js_marker_fails(self):
		error = run(**{"js/net/src/internal.ts": ("\tunused:", "\t// private: plumbing\n\tunused:")})
		self.assertIn("internal.ts:5: marker is not on an item", error)


if __name__ == "__main__":
	unittest.main()
