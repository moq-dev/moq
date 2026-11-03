# [L] Adopt formatter defaults after evaluating Bun

## Goal

Every language uses its formatter's default style instead of repository-specific
formatting overrides, and editors, `just check`, and `just fix` agree. YAML and
Markdown are formatted too, and remark is retired. Evaluate whether `bun fmt`
and `bun lint` replace Biome before the coordinated reformat; adopt defaults
even if the evaluation keeps Biome.

## Plan

Remove style overrides, including indentation, line widths, and quoting, from
formatter configuration, command flags, and EditorConfig. Keep lint rules,
parser capabilities, and exclusions that protect generated, vendored, or
byte-sensitive files. Use the selected formatter's defaults where a language
has no single standard style; do not invent a replacement repository style.

Coordinate the repository-wide reformat with the Bun decision so files do not
first change to Biome defaults and then to Bun's. Native languages join the
same migration even though Bun does not format them. Until then, keep the
current formatting configuration and checks.

The existing Bun evaluation remains conditional on a stable `bun lint` in
nixpkgs. Compare lint coverage, equivalents for every custom ban, and wall
time of lint and format steps on this workspace. Switch only if lint coverage
improves; formatting speed alone does not justify splitting the tools. If
Biome wins, use its defaults and the existing native formatters' defaults.

This absorbs the former YAML and Markdown formatting quests. The formatter
chosen by the evaluation formats both in `just check` and `just fix`. Retire
remark entirely, including its config, script, and packages, carrying its
exclusions into the chosen formatter. Preserve frontmatter support. Enable
applicable Markdown lint rules and list the remark checks that are dropped;
the 2026-10-29 decision accepted retiring remark without full lint parity.
If Biome remains, use 2.6.4 or later for its Markdown list and table fixes.

Land the configuration, tool integration, and mechanical reformat together in
a quiet window. Verify that the formatters are idempotent and the diff changes
no program, fixture, or document meaning. Check the affected language gates,
YAML consumers (including workflows, Dart, and nfpm), `quest check`, and the
quest link-list merge driver. Agent instructions and skills change only in
format. Public API and wire formats do not change.

Decided with the maintainer on 2026-11-03:

- Style: indentation defaults only / ✅ full formatter defaults, removing
  custom style choices rather than continuing the tabs-versus-spaces dispute.
- Timing: reformat now / ✅ coordinate with the Bun decision, avoiding two
  formatting passes over the same files.
- If Bun loses: make cleanup conditional on switching / ✅ adopt the current
  formatters' defaults anyway; standard formatting is the goal.
- YAML and Markdown: keep their earlier separate schedule / ✅ fold both into
  this migration, avoiding repeated reformats. Their formatter choice is now
  part of the Bun evaluation; retiring remark remains settled.
- Milestone: m3 / ✅ m2, moved here at the maintainer's request.

## Required

- [Wait for a stable bun lint](/quest/m3/bun-lint-release.md) - `bun lint`
  ships in a stable Bun that nixpkgs carries

## Related

- [Typecheck with bun check](/quest/m2/bun-check.md) - the same Bun toolchain
