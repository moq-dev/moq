//! Parsing one quest document into the facts every rule reads.
//!
//! This is a real Markdown AST rather than line matching. The shell version
//! that came before it was defeated by a fence longer than three backticks
//! (which inverted its fence tracking and silently skipped the rest of the
//! file), by a `Required` bullet that wrapped onto a second line (which moved
//! the link out of reach of the check that exists to catch it), and by
//! reference-style links (which produced a rendered dependency with no edge).
//! None of those are special cases here; the parser simply reports the
//! structure that Markdown actually has.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Where a link sits inside its `## Section`, which is what separates a
/// dependency edge from prose that merely mentions one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
	/// The link opens a list item, ignoring any emphasis around it. This is the
	/// shape every `Required` blocker and every `Quests` entry must have.
	Entry,
	/// Somewhere else inside a list item: a sentence that happens to link.
	Inside,
	/// Outside any list item.
	Prose,
}

/// One Markdown link, located precisely enough to be a graph edge.
#[derive(Clone, Debug)]
pub struct Link {
	/// 1-based source line, for findings.
	pub line: usize,
	/// The enclosing `## Heading`, or `None` above the first one.
	pub section: Option<String>,
	/// The destination exactly as written, fragment included.
	pub target: String,
	/// Where the link sits in its section; see [`Position`].
	pub position: Position,
}

/// One `# ` or `## ` heading as the rules need to see it.
#[derive(Clone, Debug)]
pub struct Heading {
	/// 1-based source line, for findings.
	pub line: usize,
	/// The rendered heading text, trimmed.
	pub text: String,
	/// The source really is `# Text` or `## Text` on its own line. Setext and
	/// decorated headings render the same, but literal syntax is the contract.
	pub literal: bool,
}

/// One parsed quest document: the structure the rules read, nothing else.
#[derive(Clone, Debug)]
pub struct Doc {
	/// Repository-relative, e.g. `quest/m0/one.md`.
	pub path: PathBuf,
	/// The document's `# ` title, used for a quest's t-shirt size.
	pub title: Option<Heading>,
	/// `## ` headings only. Deeper levels are free-form prose structure.
	pub headings: Vec<Heading>,
	/// Every link in the document, in source order.
	pub links: Vec<Link>,
	/// Sections holding at least one top-level list item, so a heading left
	/// behind by its last entry can be told from one that still has entries.
	pub sections_with_entries: Vec<String>,
}

impl Doc {
	/// Whether the document has this exact `## ` heading.
	pub fn has(&self, heading: &str) -> bool {
		self.headings.iter().any(|h| h.text == heading)
	}

	/// A questline is a `README.md`; everything else is an executable quest.
	pub fn is_questline(&self) -> bool {
		self.path.file_name().is_some_and(|n| n == "README.md")
	}

	/// The questline directory this document belongs to. A questline is a
	/// DIRECTORY, so its own entry sits one level further out than a quest's:
	/// `quest/m2/drain/README.md` belongs to `quest/m2`, not to `quest/m2/drain`.
	pub fn owner(path: &Path) -> PathBuf {
		let parent = path.parent().unwrap_or(Path::new(""));
		if path.file_name().is_some_and(|n| n == "README.md") {
			parent.parent().unwrap_or(Path::new("")).to_path_buf()
		} else {
			parent.to_path_buf()
		}
	}

	/// Read and parse `<root>/<path>`; `path` stays repository-relative.
	pub fn parse(root: &Path, path: PathBuf) -> Result<Doc> {
		let text = std::fs::read_to_string(root.join(&path)).with_context(|| format!("reading {}", path.display()))?;
		Ok(Self::from_str(path, &text))
	}

	/// Parse already-loaded Markdown; this is the whole parser, `parse` just adds IO.
	pub fn from_str(path: PathBuf, text: &str) -> Doc {
		let lines = LineIndex::new(text);
		let text_src = text;

		let mut title = None;
		let mut headings = Vec::new();
		let mut links = Vec::new();
		let mut sections_with_entries = Vec::new();
		let mut section: Option<String> = None;

		// Depth of nesting, so a sub-list inside an entry does not read as a
		// second entry, and so `fresh` tracks the innermost item.
		let mut item_depth = 0usize;
		// Entries are the flat, top-level list. A bullet nested under a prose
		// lead-in, or one inside a block quote, is illustration - counting it
		// would let `- evidence:` + an indented link become a real blocker.
		let mut quote_depth = 0usize;
		// Nothing but emphasis has been seen since the current item opened, so a
		// link here is the item's opening link. `**[Blocker](/quest/b.md)**` is
		// still an opener; `A customer who justifies [b](/quest/b.md)` is not.
		let mut fresh = false;
		let mut heading: Option<(HeadingLevel, usize, String)> = None;

		let mut options = Options::empty();
		options.insert(Options::ENABLE_STRIKETHROUGH);
		options.insert(Options::ENABLE_TABLES);

		for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
			match event {
				Event::Start(Tag::Heading { level, .. }) if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) => {
					heading = Some((level, lines.line_of(range.start), String::new()));
				}
				Event::End(TagEnd::Heading(level)) if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) => {
					if let Some((start_level, line, text)) = heading.take() {
						debug_assert_eq!(start_level, level);
						let text = text.trim().to_string();
						// Exact, not trimmed: `rg '^## Required$'` does not match a
						// line with trailing spaces either, and this rule exists
						// precisely so the two can never disagree.
						let prefix = if level == HeadingLevel::H1 { "#" } else { "##" };
						let parsed = Heading {
							line,
							literal: lines.text_of(text_src, line) == format!("{prefix} {text}"),
							text: text.clone(),
						};
						if level == HeadingLevel::H1 {
							title = Some(parsed);
						} else {
							section = Some(text);
							headings.push(parsed);
						}
					}
					item_depth = 0;
					fresh = false;
				}
				Event::Start(Tag::BlockQuote(..)) => {
					quote_depth += 1;
					fresh = false;
				}
				Event::End(TagEnd::BlockQuote(..)) => {
					quote_depth = quote_depth.saturating_sub(1);
					fresh = false;
				}
				Event::Start(Tag::Item) => {
					item_depth += 1;
					fresh = true;
					if item_depth == 1
						&& quote_depth == 0
						&& let Some(name) = &section
						&& !sections_with_entries.iter().any(|s| s == name)
					{
						sections_with_entries.push(name.clone());
					}
				}
				Event::End(TagEnd::Item) => {
					item_depth = item_depth.saturating_sub(1);
					fresh = false;
				}
				Event::Start(Tag::Link { dest_url, .. }) => {
					// Reference-style links arrive here already resolved to their
					// destination, so they carry an edge like any other link.
					// Only a top-level, unquoted item can hold an entry.
					let position = if item_depth == 0 {
						Position::Prose
					} else if fresh && item_depth == 1 && quote_depth == 0 {
						Position::Entry
					} else {
						Position::Inside
					};
					fresh = false;
					links.push(Link {
						line: lines.line_of(range.start),
						section: section.clone(),
						target: dest_url.to_string(),
						position,
					});
				}
				// Emphasis wraps an opening link without displacing it, and so does
				// the paragraph a LOOSE list puts around every item: clearing on
				// its START would classify every entry in a blank-line-separated
				// list as mid-sentence prose. Its END does clear, so a link that
				// opens a SECOND paragraph is inside the item, not opening it.
				Event::Text(ref t) | Event::Code(ref t) => {
					if let Some((_, _, buf)) = heading.as_mut() {
						buf.push_str(t);
					}
					if !t.trim().is_empty() {
						fresh = false;
					}
				}
				Event::Start(Tag::Emphasis | Tag::Strong | Tag::Paragraph)
				| Event::End(TagEnd::Emphasis | TagEnd::Strong) => {}
				Event::End(TagEnd::Paragraph) => fresh = false,
				Event::SoftBreak | Event::HardBreak => {}
				_ => {
					if item_depth > 0 {
						fresh = false;
					}
				}
			}
		}

		Doc {
			path,
			title,
			headings,
			links,
			sections_with_entries,
		}
	}
}

/// Byte offset -> 1-based line number, so findings can name a line.
struct LineIndex {
	starts: Vec<usize>,
}

impl LineIndex {
	fn new(text: &str) -> LineIndex {
		let mut starts = vec![0];
		starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
		LineIndex { starts }
	}

	fn line_of(&self, offset: usize) -> usize {
		self.starts.partition_point(|&start| start <= offset)
	}

	/// The source of a 1-based line, without its newline.
	fn text_of<'a>(&self, text: &'a str, line: usize) -> &'a str {
		let start = self.starts.get(line - 1).copied().unwrap_or(text.len());
		let end = self.starts.get(line).copied().unwrap_or(text.len());
		text[start..end].trim_end_matches('\n')
	}
}
