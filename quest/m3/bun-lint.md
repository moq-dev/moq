# [S] Evaluate bun fmt and bun lint

## Goal

A recorded decision on whether `bun fmt` and `bun lint` replace Biome for the
workspace's JavaScript, TypeScript, JSON, CSS, YAML, and Markdown, measured on
this workspace, and the switch if it wins.

## Plan

Biome formats and lints in one tool and one `biome.jsonc`. `bun lint` reads
ESLint and oxlint configs, not Biome's, so a switch rewrites the rule set,
including any custom bans. Its draw is type-aware rules, which Biome covers
only partly; `bun fmt` alone is faster, but formatting time is not a
bottleneck. Compare on `main`: wall time of the lint and format steps in
`just check`, findings each tool reports that the other misses, and whether
every custom ban has an equivalent. Switch only if lint coverage improves;
formatting speed alone does not justify splitting the tools.

Decided with the maintainer on 2026-10-29: stay on Biome until `bun lint` is
stable, then evaluate (✅ evaluate after a stable release / switch now /
never), in m3, as in moq.pro.

## Required

- [Wait for a stable bun lint](/quest/m3/bun-lint-release.md) - `bun lint`
  ships in a stable Bun that nixpkgs carries

## Related

- [Typecheck with bun check](/quest/m2/bun-check.md) - the same bun bump
- [moq-dev/moq.pro `quest/m3/bun-lint.md`](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/bun-lint.md) -
  the same evaluation in moq.pro
