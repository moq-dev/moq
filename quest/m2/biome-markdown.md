# [M] Format Markdown with Biome

## Goal

Biome formats every Markdown file in `just check` and `just fix`, and remark
is gone: `.remarkrc.mjs`, `.remarkignore`, `sh/markdown.sh`, and the remark
packages are removed, with `.remarkignore`'s exclusions carried into
`biome.jsonc`.

## Plan

Retire remark entirely in the same PR rather than keep `remark-lint` for
linting (decided 2026-10-29). That drops the `remark-preset-lint-consistent`
and `-recommended` checks, which Biome's Markdown lint rules (all nursery as
of 2.6.4) do not yet replace; enable whichever nursery Markdown rules pass
cleanly and list the dropped remark checks in the PR. Biome's style also
differs from `.remarkrc.mjs` where Prettier's does (for example `_emphasis_`
rather than `*emphasis*`); accept Biome's style unless an option keeps the
current one. Enable `markdown.parser.frontmatter`, since remark parsed
frontmatter.

Use Biome 2.6.4 or later: 2.6.3 fixed the paragraph-before-list blank-line bug
([biomejs/biome#12524](https://github.com/biomejs/biome/issues/12524)), and
2.6.4 fixes blockquote/table-before-list, deep list nesting, and table
idempotence. About 1,018 tracked Markdown files, quests among them, so land
the reformat in a quiet window with few open PRs touching Markdown: config,
removal, and reformat in one PR whose content diff is reviewed as formatting
only. Check that `quest check` and the link-list merge driver accept the
reformatted lists, and that `AGENTS.md` and skill files change only in format.

Decided with the maintainer on 2026-10-29:
- Adopt Biome's Markdown formatter: ✅ yes, in a quiet window / keep remark
  and skip Biome Markdown
- remark: ✅ retire entirely / Biome formats and remark-lint stays until
  Biome's Markdown rules leave nursery
- Milestone: ✅ m2

## Related

- [moq-dev/moq.pro `quest/m2/biome-markdown.md`](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/biome-markdown.md) -
  the same formatter in moq.pro, which has no remark to retire
