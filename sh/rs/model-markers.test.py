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
	"js/net/src/index.ts": 'export * as Track from "./track.ts";\nexport type { Box, Shape } from "./box.ts";\n',
	"js/net/src/box.ts": "export class Box {}\nexport interface Shape {}\n",
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
		'import { wireOf } from "../wire.ts";\n'
		'import * as Box from "../box.ts";\n'
		'import { hooks } from "../internal.ts";\n'
		"hooks.used();\n"
		"let shape: Box.Shape;\n"
		"wireOf({}).subscribe();\n"
		"subscriber.fork();\n"
	),
	"js/net/src/lite/session.test.ts": 'import { hooks } from "../internal.ts";\nhooks.unused();\n',
}

HOOKS = 'import { hooks } from "../internal.ts";\nhooks.used();'


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

	def test_marker_above_a_wrapped_attribute_passes(self):
		wrapped = '// private: test\n#[cfg_attr(\n\tnot(feature = "async"),\n\tallow(dead_code, reason = "why")\n)]\npub(crate) struct Open'
		self.assertIsNone(run(**{"rs/moq-net/src/model/group.rs": ("// private: test\npub(crate) struct Open", wrapped)}))

	def test_delimiters_in_attribute_strings_and_comments_are_ignored(self):
		wrapped = '// private: test\n#[cfg_attr(\n\tnot(feature = "async"), /* /* ) */ ) */\n\tallow(dead_code, reason = r#"why " )"#)\n)]\npub(crate) struct Open'
		self.assertIsNone(run(**{"rs/moq-net/src/model/group.rs": ("// private: test\npub(crate) struct Open", wrapped)}))

	def test_code_ending_like_an_attribute_does_not_lend_its_marker(self):
		table = "// private: test\n#[cfg(test)]\npub(crate) const TABLE: [u8; 1] = [\n\tu8::pow(1, 1\n)];\npub(crate) struct Inner"
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", table)})
		self.assertIn("group.rs:6: `pub(crate)`", error)

	def test_code_ending_in_a_bare_attribute_close_does_not_lend_its_marker(self):
		table = "// private: test\n#[cfg(test)]\npub(crate) const TABLE: [u8; 1] = [u8::pow(\n\t1, 1\n)]\npub(crate) struct Inner"
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", table)})
		self.assertIn("group.rs:6: `pub(crate)`", error)

	def test_stale_rust_marker_fails(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("pub(super) struct Inner", "// private: test\npub(super) struct Inner")})
		self.assertIn("group.rs:1: marker is not on an item", error)

	def test_session_reaching_an_unmarked_hook_fails(self):
		error = run(**{"js/net/src/lite/session.ts": ("hooks.used();", "hooks.used();\nhooks.unused();")})
		self.assertIn("internal.ts:5: `hooks.unused`", error)

	def test_aliased_hook_read_fails(self):
		error = run(**{"js/net/src/lite/session.ts": (HOOKS, 'import { hooks as h } from "../internal.ts";\nh.used();\nh.unused();')})
		self.assertIn("internal.ts:5: `hooks.unused`", error)

	def test_namespaced_hook_read_fails(self):
		namespaced = 'import * as Internal from "../internal.ts";\nInternal.hooks.used();\nInternal.hooks.unused();'
		error = run(**{"js/net/src/lite/session.ts": (HOOKS, namespaced)})
		self.assertIn("internal.ts:5: `hooks.unused`", error)

	def test_destructured_hooks_fail(self):
		error = run(**{"js/net/src/lite/session.ts": ("hooks.used();", "const { unused } = hooks;")})
		self.assertIn("session.ts:1: read `hooks` only as `hooks.<name>`", error)

	def test_destructured_namespace_fails(self):
		error = run(**{"js/net/src/lite/session.ts": ("let shape: Box.Shape;", "const { Box: Made } = Box;")})
		self.assertIn("session.ts:1: read `Box` only as `Box.<name>`", error)

	def test_default_import_of_a_model_module_fails(self):
		error = run(**{"js/net/src/lite/session.ts": ('import * as Box from "../box.ts";', 'import Box, { Shape } from "../box.ts";')})
		self.assertIn("import a model module by name or namespace only", error)

	def test_namespace_value_reach_of_a_type_export_fails(self):
		error = run(**{"js/net/src/lite/session.ts": ("let shape: Box.Shape;", "new Box.Box();")})
		self.assertIn("box.ts:1: `Box`", error)

	def test_type_namespace_reach_of_a_type_export_passes(self):
		typed = 'import type * as Box from "../box.ts";\nlet box: Box.Box;'
		self.assertIsNone(run(**{"js/net/src/lite/session.ts": ('import * as Box from "../box.ts";', typed)}))

	def test_plumbing_on_rust_fails(self):
		error = run(**{"rs/moq-net/src/model/group.rs": ("private: test", "private: plumbing")})
		self.assertIn("group.rs:5: unknown private marker 'plumbing'", error)

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
