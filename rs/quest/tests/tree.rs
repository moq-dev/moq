//! Every rule, each proven to actually fail, and the readiness the same tree
//! answers.
//!
//! A validator that silently stopped enforcing a rule is indistinguishable from
//! a clean tree, so each case starts from the same valid fixture and breaks
//! exactly one thing. The cases that must still PASS matter just as much: the
//! shell version this replaced was rewritten precisely because it rejected
//! legitimate Markdown and accepted the shapes it was written to catch.

use std::path::Path;

use tempfile::TempDir;

const ROOT_README: &str = "\
# Quests

## Goal

The permanent root questline.

## Quests

- [dev](/quest/dev/README.md)
";

const DEV_README: &str = "\
# dev

## Goal

A milestone.

## Quests

- [Line](/quest/dev/line/README.md)
";

const LINE_README: &str = "\
# Line

## Goal

A questline.

## Quests

- [One](/quest/dev/line/one.md)
- [Two](/quest/dev/line/two.md)
";

const ONE: &str = "\
# [S] One

## Goal

A quest.
";

const TWO: &str = "\
# [S] Two

## Goal

Another quest.

## Required

- [One](/quest/dev/line/one.md) - must finish first
";

/// A minimal but complete tree: root questline -> milestone -> questline -> two
/// quests, one blocking the other.
struct Tree(TempDir);

impl Tree {
	fn new() -> Tree {
		let tree = Tree(TempDir::new().expect("tempdir"));
		tree.write("quest/README.md", ROOT_README);
		tree.write("quest/dev/README.md", DEV_README);
		tree.write("quest/dev/line/README.md", LINE_README);
		tree.write("quest/dev/line/one.md", ONE);
		tree.write("quest/dev/line/two.md", TWO);
		tree
	}

	fn path(&self) -> &Path {
		self.0.path()
	}

	fn write(&self, rel: &str, body: &str) -> &Tree {
		let path = self.path().join(rel);
		std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
		std::fs::write(path, body).expect("write");
		self
	}

	fn append(&self, rel: &str, body: &str) -> &Tree {
		let path = self.path().join(rel);
		let existing = std::fs::read_to_string(&path).expect("read");
		std::fs::write(path, format!("{existing}{body}")).expect("write");
		self
	}

	fn findings(&self) -> Vec<String> {
		quest::check(self.path())
			.expect("check")
			.iter()
			.map(ToString::to_string)
			.collect()
	}

	/// The rendered blocker chain: one line per blocker, nesting indented.
	fn blockers(&self, path: &str) -> Vec<String> {
		quest::ready::blockers(self.path(), Path::new(path))
			.expect("blockers")
			.iter()
			.flat_map(|blocker| blocker.to_string().lines().map(str::to_owned).collect::<Vec<_>>())
			.collect()
	}

	fn ready(&self) -> Vec<String> {
		quest::ready::quests(self.path())
			.expect("ready")
			.iter()
			.map(|path| path.display().to_string())
			.collect()
	}

	#[track_caller]
	fn accepts(&self) {
		let findings = self.findings();
		assert!(findings.is_empty(), "expected the tree to pass, got: {findings:#?}");
	}

	#[track_caller]
	fn without(&self, unexpected: &str) {
		let findings = self.findings();
		assert!(
			!findings.iter().any(|f| f.contains(unexpected)),
			"expected NO finding containing {unexpected:?}, got: {findings:#?}"
		);
	}

	#[track_caller]
	fn rejects(&self, expected: &str) {
		let findings = self.findings();
		assert!(
			findings.iter().any(|f| f.contains(expected)),
			"expected a finding containing {expected:?}, got: {findings:#?}"
		);
	}
}

/// The fixture itself has to pass, or every case below proves nothing.
#[test]
fn baseline_is_valid() {
	Tree::new().accepts();
}

#[test]
fn dangling_absolute_link() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Related\n\n- [Gone](/quest/dev/line/gone.md) - completed and deleted\n",
	);
	tree.rejects("link does not resolve: /quest/dev/line/gone.md");
}

/// Relative links escape the tree (CLAUDE.md points at ../CONTRIBUTING.md), so
/// they resolve against the LINKING FILE's directory. The pair of cases pins the
/// direction: resolving against the wrong base would flip both verdicts.
#[test]
fn relative_link_resolves_against_the_linking_file() {
	let tree = Tree::new();
	tree.write("CLAUDE.md", "# Guide\n");
	tree.append(
		"quest/dev/line/one.md",
		"\n## Plan\n\nSee [the guide](../../../CLAUDE.md).\n",
	);
	tree.accepts();
}

#[test]
fn relative_link_above_the_repository_root() {
	let tree = Tree::new();
	tree.write("CLAUDE.md", "# Guide\n");
	// One `..` too many, which is exactly what a file flattened up a level
	// keeps: it still renders, and points at nothing.
	tree.append(
		"quest/dev/line/one.md",
		"\n## Plan\n\nSee [the guide](../../../../CLAUDE.md).\n",
	);
	tree.rejects("link does not resolve: ../../../../CLAUDE.md");
}

/// Quests reference each other root-absolutely. A relative one renders fine, so
/// nothing else would notice - but it is invisible to the dependency graph.
#[test]
fn relative_link_to_a_quest() {
	let tree = Tree::new();
	tree.append("quest/dev/line/one.md", "\n## Related\n\n- [Two](two.md) - a sibling\n");
	tree.rejects("link to a quest must be root-absolute: two.md (write /quest/dev/line/two.md)");
}

/// Templates inside fenced blocks are illustrations. Flagging CLAUDE.md's own
/// example would make this a check everyone learns to skip.
#[test]
fn fenced_templates_are_not_links() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Plan\n\n```markdown\n## Required\n\n- [Blocker](/quest/foo/bar.md) - must finish first\n```\n",
	);
	tree.accepts();
}

/// A fence longer than three backticks may contain shorter ones, and `~~~` is a
/// fence too. Miscounting either inverts the fence state and silently skips the
/// REST OF THE FILE - the worst failure this tool has, because it looks clean.
#[test]
fn nested_and_tilde_fences_do_not_leak() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Plan\n\n````markdown\n```bash\njust check\n```\n````\n\n~~~text\n```\n~~~\n\n## Requires\n\n- [Gone](/quest/dev/line/gone.md) - typo'd heading and a dangling link\n",
	);
	tree.rejects("unknown '## Requires'");
	tree.rejects("link does not resolve: /quest/dev/line/gone.md");
}

#[test]
fn missing_goal() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/one.md",
		"# [S] One\n\n## Plan\n\nA quest with no stated outcome.\n",
	);
	tree.rejects("missing '## Goal'");
}

#[test]
fn quest_title_needs_a_size() {
	let tree = Tree::new();
	tree.write("quest/dev/line/one.md", &ONE.replace("# [S] One", "# One"));
	tree.rejects("quest title must be '# [XS|S|M|L|XL] Title'");
}

#[test]
fn quest_title_accepts_xl() {
	let tree = Tree::new();
	tree.write("quest/dev/line/one.md", &ONE.replace("# [S] One", "# [XL] One"));
	tree.accepts();
}

#[test]
fn quest_title_rejects_xxl() {
	let tree = Tree::new();
	tree.write("quest/dev/line/one.md", &ONE.replace("# [S] One", "# [XXL] One"));
	tree.rejects("quest title must be '# [XS|S|M|L|XL] Title'");
}

/// The closed vocabulary exists for this: readiness greps `## Required`
/// literally, so a typo makes a blocked quest read as ready and fails nowhere.
#[test]
fn typo_in_a_heading() {
	let tree = Tree::new();
	tree.write("quest/dev/line/two.md", &TWO.replace("## Required", "## Requires"));
	tree.rejects("unknown '## Requires'");
}

#[test]
fn quest_with_a_questline_index() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Quests\n\n- [Two](/quest/dev/line/two.md)\n",
	);
	tree.rejects("only a README may have '## Quests'");
}

/// A README whose last child merged is the line's own remaining work: a leaf
/// quest, sized and listed as ready like any other.
#[test]
fn readme_without_an_index_is_a_quest() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/sub/README.md",
		"# Sub\n\n## Goal\n\nThe end-to-end test once every child has merged.\n",
	);
	tree.append("quest/dev/line/README.md", "- [Sub](/quest/dev/line/sub/README.md)\n");
	tree.rejects("quest title must be");
	tree.write(
		"quest/dev/line/sub/README.md",
		"# [S] Sub\n\n## Goal\n\nThe end-to-end test once every child has merged.\n",
	);
	tree.accepts();
	assert!(tree.ready().contains(&"quest/dev/line/sub/README.md".to_string()));
}

/// Completing a questline's last quest deletes the directory. A bare `## Quests`
/// heading otherwise satisfies the questline rule and leaves the husk standing.
/// (The other direction - a heading holding a non-quest bullet - is
/// `questline_listing_no_quest`; one rule, both shapes.)
#[test]
fn empty_questline_index() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/husk/README.md",
		"# Husk\n\n## Goal\n\nIts last quest was completed.\n\n## Quests\n",
	);
	tree.append("quest/dev/README.md", "- [Husk](/quest/dev/husk/README.md)\n");
	tree.rejects("lists no quest");
}

/// The absence of `## Required` means ready. A heading left behind by its last
/// blocker reads as blocked to every readiness check, forever.
#[test]
fn empty_required_section() {
	let tree = Tree::new();
	tree.append("quest/dev/line/one.md", "\n## Required\n");
	tree.rejects("'## Required' is empty");
}

#[test]
fn unlisted_quest() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/three.md",
		"# [S] Three\n\n## Goal\n\nA quest nobody indexed.\n",
	);
	tree.rejects("not listed in quest/dev/line/README.md's '## Quests'");
}

/// Quests are indexed where they sit, so a milestone cannot reach past its own
/// questlines to list a grandchild.
#[test]
fn questline_listing_a_grandchild() {
	let tree = Tree::new();
	tree.append("quest/dev/README.md", "- [One](/quest/dev/line/one.md)\n");
	tree.rejects("does not sit under this questline");
}

#[test]
fn quest_listed_twice() {
	let tree = Tree::new();
	tree.append("quest/dev/line/README.md", "- [One again](/quest/dev/line/one.md)\n");
	tree.rejects("lists /quest/dev/line/one.md twice");
}

#[test]
fn relative_index_entry() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/README.md",
		&LINE_README.replace("(/quest/dev/line/one.md)", "(one.md)"),
	);
	tree.rejects("must be a root-absolute /quest/... link: one.md");
}

/// The index points readers at work to pick up, so a target that merely exists
/// is not enough: quest/CLAUDE.md is a file under quest/ that is not a quest.
#[test]
fn index_entry_that_is_not_a_quest() {
	let tree = Tree::new();
	tree.write("quest/CLAUDE.md", "# Contract\n");
	tree.append("quest/README.md", "- [Contract](/quest/CLAUDE.md)\n");
	tree.rejects("lists /quest/CLAUDE.md, which is not a quest document");
}

/// The index is a list of entries, not prose that happens to link.
#[test]
fn prose_under_the_index() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/README.md",
		"\nSee also [One](/quest/dev/line/one.md).\n",
	);
	tree.rejects("a Quests entry must open its bullet");
}

#[test]
fn direct_required_cycle() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- [Two](/quest/dev/line/two.md) - must finish first\n",
	);
	tree.rejects("Required cycle:");
}

/// A quest may require a whole questline, so the deadlock can span the README.
/// The cycle here runs strictly OUTSIDE-IN: `outer` (in dev) requires the line
/// questline, and `three` inside that questline requires `outer` back. No quest
/// requires its own questline, so containment edges are the only thing that can
/// close it.
#[test]
fn cycle_through_a_questline() {
	let tree = Tree::new();
	tree.append("quest/dev/README.md", "- [Outer](/quest/dev/outer.md)\n");
	tree.write(
		"quest/dev/outer.md",
		"# [S] Outer\n\n## Goal\n\nBlocked on a whole questline.\n\n## Required\n\n- [Line](/quest/dev/line/README.md) - the whole questline must finish\n",
	);
	tree.append("quest/dev/line/README.md", "- [Three](/quest/dev/line/three.md)\n");
	tree.write(
		"quest/dev/line/three.md",
		"# [S] Three\n\n## Goal\n\nInside the questline that blocks it.\n\n## Required\n\n- [Outer](/quest/dev/outer.md) - must finish first\n",
	);
	tree.rejects(
		"Required cycle: quest/dev/line/README.md -> quest/dev/line/three.md -> quest/dev/outer.md -> quest/dev/line/README.md",
	);
}

/// A `#fragment` addresses a place inside a file, not a different file. Keeping
/// it on made a phantom graph node that no quest could match, so a cycle through
/// an anchored link went unreported.
#[test]
fn cycle_through_an_anchored_link() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- [Two](/quest/dev/line/two.md#plan) - must finish first\n",
	);
	tree.rejects("Required cycle:");
}

/// Reference-style links render as real dependencies, so they must carry real
/// edges. A line-oriented parser saw no `](` here and produced none.
#[test]
fn cycle_through_a_reference_style_link() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- [Two][two] - must finish first\n\n[two]: /quest/dev/line/two.md\n",
	);
	tree.rejects("Required cycle:");
}

/// moq-dev/moq.pro#1170: a plain-text external condition that happens to link a
/// questline mid-sentence reads as context but IS a dependency edge.
#[test]
fn required_link_mid_sentence() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- A customer who also justifies [the line](/quest/dev/line/README.md).\n",
	);
	tree.rejects("mid-sentence");
}

/// The same failure, reflowed onto a second line. The link is no longer on the
/// bullet's first line, which is all it took to slip past the shell version.
#[test]
fn required_link_on_a_wrapped_bullet() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- A customer who also justifies\n  [the line](/quest/dev/line/README.md).\n",
	);
	tree.rejects("mid-sentence");
}

/// The other half of that rule: an external condition with no link at all is the
/// shape CLAUDE.md prescribes, and must stay legal.
#[test]
fn required_external_condition() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- A customer who justifies the work.\n",
	);
	tree.accepts();
}

/// A LOOSE list - blank lines between entries - wraps every item in a paragraph.
/// Treating that paragraph as text would classify every entry in the list as
/// mid-sentence prose, which is a false positive on ordinary Markdown and would
/// fire on every blocker and every index entry at once.
#[test]
fn loose_index_list() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/README.md",
		"# Line\n\n## Goal\n\nA questline.\n\n## Quests\n\n- [One](/quest/dev/line/one.md)\n\n- [Two](/quest/dev/line/two.md)\n",
	);
	tree.accepts();
}

#[test]
fn loose_required_list() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- [Two](/quest/dev/line/two.md) - must finish first\n\n- A customer who justifies the work.\n",
	);
	// The edge registered (hence the cycle) without reading as prose.
	tree.rejects("Required cycle:");
	tree.without("mid-sentence");
}

/// The other side of the same seam: a link opening a SECOND paragraph is inside
/// the item, not opening it.
#[test]
fn required_link_in_a_later_paragraph() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- A customer who justifies the work.\n\n  [The line](/quest/dev/line/README.md) would follow.\n",
	);
	tree.rejects("mid-sentence");
}

/// A blocker written with emphasis still opens its bullet. Rejecting it would
/// be a false positive on legitimate Markdown.
#[test]
fn required_link_with_emphasis() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- **[Two](/quest/dev/line/two.md)** - must finish first\n",
	);
	tree.rejects("Required cycle:");
}

/// A setext underline renders as an H2 and this validator would read the quest
/// as blocked - but readiness greps `^## Required$` and would call it READY.
/// Two tools disagreeing about the same file is the whole failure.
#[test]
fn setext_heading() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\nRequired\n--------\n\n- [Two](/quest/dev/line/two.md) - must finish first\n",
	);
	tree.rejects("must be written literally as '## Required'");
}

#[test]
fn decorated_heading() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## `Required`\n\n- [Two](/quest/dev/line/two.md) - must finish first\n",
	);
	tree.rejects("must be written literally as '## Required'");
}

/// A bullet nested under a prose lead-in is illustration, not a blocker. Reading
/// it as one is #1170 again, wearing an indent.
#[test]
fn nested_required_entry() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- Customer evidence:\n  - [The line](/quest/dev/line/README.md)\n",
	);
	tree.rejects("mid-sentence");
}

#[test]
fn blockquoted_required_entry() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- Quoting the old plan:\n\n  > - [The line](/quest/dev/line/README.md)\n",
	);
	tree.rejects("mid-sentence");
}

/// Repeated `..` must not cancel each other on the way up. Popping
/// unconditionally let a link climb above the root and walk back down to a real
/// file, so a badly flattened path resolved and reported nothing.
#[test]
fn repeated_parent_components() {
	let tree = Tree::new();
	tree.write("CLAUDE.md", "# Guide\n");
	tree.append(
		"quest/dev/line/one.md",
		"\n## Plan\n\nSee [the guide](../../../../../CLAUDE.md).\n",
	);
	tree.rejects("link does not resolve: ../../../../../CLAUDE.md");
}

/// Escaping the root must fail even when the joined path happens to exist:
/// the repository's own directory name (or a sibling worktree) sits beside the
/// root, so `<root>/../<name>/CLAUDE.md` is a real file that readers of the
/// repository-relative link can never reach.
#[test]
fn escaped_link_resolving_beside_the_root() {
	let tree = Tree::new();
	tree.write("CLAUDE.md", "# Guide\n");
	let name = tree.path().file_name().unwrap().to_str().unwrap();
	tree.append(
		"quest/dev/line/one.md",
		&format!("\n## Plan\n\nSee [the guide](../../../../{name}/CLAUDE.md).\n"),
	);
	tree.rejects(&format!("link does not resolve: ../../../../{name}/CLAUDE.md"));
}

/// A bullet is not an entry. `- TBD` satisfies "the heading has a list" while
/// leaving a questline that should have been deleted with its last quest.
#[test]
fn questline_listing_no_quest() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/husk/README.md",
		"# Husk\n\n## Goal\n\nIts last quest was completed.\n\n## Quests\n\n- TBD\n",
	);
	tree.append("quest/dev/README.md", "- [Husk](/quest/dev/husk/README.md)\n");
	tree.rejects("lists no quest");
}

#[test]
fn empty_related_section() {
	let tree = Tree::new();
	tree.append("quest/dev/line/one.md", "\n## Related\n");
	tree.rejects("'## Related' is empty");
}

#[test]
fn empty_closes_section() {
	let tree = Tree::new();
	tree.append("quest/dev/line/one.md", "\n## Closes\n");
	tree.rejects("'## Closes' is empty");
}

/// Trailing whitespace is invisible in a diff and `rg '^## Required$'` does not
/// match it either, so accepting it recreates the same disagreement a setext
/// heading does.
#[test]
fn heading_with_trailing_space() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required  \n\n- [Two](/quest/dev/line/two.md) - must finish first\n",
	);
	tree.rejects("must be written literally as '## Required'");
}

/// The index and the cycle walk key on the same normalized path. A `..` left in
/// either makes a node nothing can match, so the cycle through it disappears.
#[test]
fn cycle_through_an_unnormalized_link() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- [Two](/quest/dev/line/../line/two.md) - must finish first\n",
	);
	tree.rejects("Required cycle:");
}

// Readiness: what `quest ready` reports about the same fixture. A blocker list
// is the machine-readable result, so the cases that must come back EMPTY carry
// as much weight as the ones that must not.

/// The absence of `## Required` is the whole definition of ready.
#[test]
fn ready_quest_has_no_blockers() {
	let tree = Tree::new();
	assert!(tree.blockers("quest/dev/line/one.md").is_empty());
}

#[test]
fn blocked_by_a_quest() {
	let tree = Tree::new();
	assert_eq!(tree.blockers("quest/dev/line/two.md"), ["quest/dev/line/one.md"]);
}

/// A plain-text bullet names a condition outside the repository, so nothing in
/// the tree can ever clear it: it is a blocker, printed as written. Wrapped
/// here because that is what the bullets in the tree actually look like.
#[test]
fn blocked_by_plain_text() {
	let tree = Tree::new();
	tree.append(
		"quest/dev/line/one.md",
		"\n## Required\n\n- A `moq-video` release that carries\n  the encoder\n",
	);
	assert_eq!(
		tree.blockers("quest/dev/line/one.md"),
		["A moq-video release that carries the encoder"]
	);
}

/// A questline blocker clears only when the whole line is complete, so the
/// useful answer is which of its quests are still open - all of them, since a
/// completed quest is deleted.
#[test]
fn blocked_by_a_questline() {
	let tree = Tree::new();
	tree.append("quest/dev/README.md", "- [Outer](/quest/dev/outer.md)\n");
	tree.write(
		"quest/dev/outer.md",
		"# [S] Outer\n\n## Goal\n\nBlocked on a whole questline.\n\n## Required\n\n- [Line](/quest/dev/line/README.md) - the whole questline must finish\n",
	);
	assert_eq!(
		tree.blockers("quest/dev/outer.md"),
		[
			"quest/dev/line/README.md",
			"  quest/dev/line/one.md",
			"  quest/dev/line/two.md",
		]
	);
}

/// A required QUEST does not expand: its own blockers are its readiness, and
/// running this on it is how you ask. Expanding buried the entries that were
/// asked for under a subtree repeated once per path through it.
#[test]
fn a_required_quest_is_not_expanded() {
	let tree = Tree::new();
	tree.append("quest/dev/line/README.md", "- [Three](/quest/dev/line/three.md)\n");
	tree.write(
		"quest/dev/line/three.md",
		"# [S] Three\n\n## Goal\n\nLast in the chain.\n\n## Required\n\n- [Two](/quest/dev/line/two.md) - must finish first\n",
	);
	assert_eq!(tree.blockers("quest/dev/line/three.md"), ["quest/dev/line/two.md"]);
}

/// `quest check` reports an empty `## Required` as a defect, and every reader
/// that greps for the heading calls the quest blocked. Reading it as ready here
/// would make this the one tool that disagrees.
#[test]
fn empty_required_section_still_blocks() {
	let tree = Tree::new();
	tree.append("quest/dev/line/one.md", "\n## Required\n");
	assert_eq!(
		tree.blockers("quest/dev/line/one.md"),
		["an empty '## Required' section, which blocks the quest until the heading is removed"]
	);
}

/// The listing is the query the start flow reproduces by grepping. Questlines
/// are never executed, so they are not in it, and `two.md` is blocked.
#[test]
fn ready_listing() {
	let tree = Tree::new();
	assert_eq!(tree.ready(), ["quest/dev/line/one.md"]);

	tree.append("quest/dev/README.md", "- [Outer](/quest/dev/outer.md)\n");
	tree.write("quest/dev/outer.md", "# [S] Outer\n\n## Goal\n\nReady too.\n");
	assert_eq!(tree.ready(), ["quest/dev/line/one.md", "quest/dev/outer.md"]);
}

/// `quest check` proves the graph acyclic, but readiness also runs on trees
/// nobody has checked yet - the branch that just introduced the cycle - and a
/// cycle there has to print rather than recurse forever.
#[test]
fn cycle_terminates() {
	let tree = Tree::new();
	tree.append("quest/dev/line/README.md", "- [Itself](/quest/dev/line/README.md)\n");
	tree.append("quest/dev/README.md", "- [Outer](/quest/dev/outer.md)\n");
	tree.write(
		"quest/dev/outer.md",
		"# [S] Outer\n\n## Goal\n\nBlocked on a questline that lists itself.\n\n## Required\n\n- [Line](/quest/dev/line/README.md) - the whole questline must finish\n",
	);
	assert_eq!(
		tree.blockers("quest/dev/outer.md"),
		[
			"quest/dev/line/README.md",
			"  quest/dev/line/one.md",
			"  quest/dev/line/two.md",
			"  quest/dev/line/README.md",
		]
	);
}

#[test]
fn ready_listing_follows_nested_priority_and_terminates_cycles() {
	let tree = Tree::new();
	tree.write("quest/dev/line/two.md", "# [S] Two\n\n## Goal\n\nReady.\n");
	tree.write("quest/dev/line/README.md", "# Line\n\n## Quests\n\n- [Two](/quest/dev/line/two.md)\n- [Self](/quest/dev/line/README.md)\n- [One](/quest/dev/line/one.md)\n");
	assert_eq!(tree.ready(), ["quest/dev/line/two.md", "quest/dev/line/one.md"]);
}

#[test]
fn ready_listing_appends_unindexed_quests() {
	let tree = Tree::new();
	tree.write("quest/dev/aaa.md", "# [S] Unindexed\n\n## Goal\n\nDiscover me.\n");
	tree.write(
		"quest/dev/blocked.md",
		"# [S] Blocked\n\n## Goal\n\nWait.\n\n## Required\n\n- External condition\n",
	);
	assert_eq!(tree.ready(), ["quest/dev/line/one.md", "quest/dev/aaa.md"]);
}

impl Tree {
	fn branch(&self, path: &str) -> Vec<String> {
		quest::branch::chain(self.path(), Path::new(path)).expect("branch")
	}

	fn branch_err(&self, path: &str) -> String {
		quest::branch::chain(self.path(), Path::new(path))
			.expect_err("expected no branch")
			.to_string()
	}
}

/// The chain is the path: the leaf, its line's README, then the long-lived
/// branches. Nothing consults git.
#[test]
fn branch_chain_of_a_quest() {
	let tree = Tree::new();
	assert_eq!(
		tree.branch("quest/dev/line/one.md"),
		["quest/dev/line/one", "quest/dev/line/README", "dev", "main"]
	);
	assert_eq!(
		tree.branch("/quest/dev/line/README.md"),
		["quest/dev/line/README", "dev", "main"]
	);
	assert_eq!(tree.branch("quest/dev/README.md"), ["dev", "main"]);
}

#[test]
fn root_has_no_branch() {
	let tree = Tree::new();
	assert!(tree.branch_err("quest/README.md").contains("no branch"));
}

/// The roadmap has no branch: a quest starts by moving under main or dev.
#[test]
fn roadmap_has_no_branch() {
	let tree = Tree::new();
	tree.write(
		"quest/next/README.md",
		"# next\n\n## Goal\n\nThe roadmap.\n\n## Quests\n\n- [Later](/quest/next/later.md)\n",
	);
	tree.write("quest/next/later.md", "# [S] Later\n\n## Goal\n\nNot started.\n");
	tree.append("quest/README.md", "- [next](/quest/next/README.md)\n");
	tree.accepts();
	let err = tree.branch_err("quest/next/later.md");
	assert!(err.contains("no branch") && err.contains("main or dev"), "{err}");
}

/// `dev` after its merge: a top-level line with nothing left is not the line's
/// own work, so it needs no size and never lists as ready.
#[test]
fn permanent_line_may_be_empty() {
	let tree = Tree::new();
	tree.write("quest/dev/README.md", "# dev\n\n## Goal\n\nEmpty after the merge.\n");
	std::fs::remove_dir_all(tree.path().join("quest/dev/line")).expect("rm");
	tree.accepts();
	assert!(tree.ready().is_empty(), "{:?}", tree.ready());
	assert_eq!(tree.branch("quest/dev/README.md"), ["dev", "main"]);
}

/// Lines nest to any depth: every branch ends in a leaf component (the quest
/// or `README`), so no line's branch is a path prefix of its children's, which
/// is the one shape git refuses.
#[test]
fn branch_chain_of_a_nested_line() {
	let tree = Tree::new();
	tree.write(
		"quest/dev/line/sub/README.md",
		"# Sub\n\n## Goal\n\nA nested line.\n\n## Quests\n\n- [Three](/quest/dev/line/sub/three.md)\n",
	);
	tree.write(
		"quest/dev/line/sub/three.md",
		"# [S] Three\n\n## Goal\n\nA nested quest.\n",
	);
	tree.append("quest/dev/line/README.md", "- [Sub](/quest/dev/line/sub/README.md)\n");
	tree.accepts();
	assert_eq!(
		tree.branch("quest/dev/line/sub/three.md"),
		[
			"quest/dev/line/sub/three",
			"quest/dev/line/sub/README",
			"quest/dev/line/README",
			"dev",
			"main"
		]
	);
}
