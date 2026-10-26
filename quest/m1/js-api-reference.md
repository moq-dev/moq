# [L] Generated JS API reference

## Goal

The doc site has an API reference for every published `@moq/*` package,
including `@moq/watch`, `@moq/publish`, `@moq/libav`, and `@moq/boy`,
which JSR cannot host because of their global type augmentation. Each package
page under `doc/lib/js/` links its reference.

## Plan

Decided 2026-10-23 (maintainer interview):

- API Extractor's doc model (`.api.json`), from the shared per-package config
  that [the checks](/quest/m1/api-extractor-checks.md) finish, is the only
  source. The reference is TSDoc next to the code, rendered; update the
  "not the reference" line in `doc/AGENTS.md` to say so.
- Our own renderer, not api-documenter: a small script turns the doc model
  into VitePress pages styled like the rest of the site.
- One page per entrypoint (such as `/lib/js/net/api`), with an anchor per
  symbol: the same split as the `.api.md` reports.
- Generated at doc build time from the checkout being built, so it always
  matches the guides beside it. Nothing generated is committed.
- JSR keeps publishing its own reference; this does not replace it.
- Rust is a non-goal: docs.rs covers it.
- The link resolver (`nameableReference` from
  [moq-dev/moq#6794](https://github.com/moq-dev/moq/pull/6794)) gets fixtures
  for a hidden and an exported declaration sharing a name, and for chains of
  aliases.

## Required

- [API Extractor checks](/quest/m1/api-extractor-checks.md) - finishes the shared config and makes every exported item documented
