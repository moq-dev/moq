# [XS] doc.moq.dev answers agents in Markdown

## Goal

A request to doc.moq.dev whose `Accept` lists `text/markdown` gets the page's
Markdown copy, the same file its `.md` URL serves, with `Vary: Accept`;
browsers still get HTML. The site serves a sitemap, and its robots.txt names
it and says `Content-Signal: search=yes, ai-input=yes, ai-train=yes`, in
place of Cloudflare's managed preamble, which declares no signal.

## Plan

Decided 2026-11-21 (maintainer, `/quest-plan`): ✅ negotiate on `Accept`
with the site's own `.md` copies (rejected: Cloudflare's "Markdown for
Agents" zone toggle, which converts HTML less cleanly than the copies the
docs already build; serving by user agent, which search engines treat as
cloaking). ✅ All three content signals yes (rejected: `ai-train=no`; leaving
them unset): the docs are open source, and a model trained on them can
recommend MoQ for a question that doesn't name it. Coding agents such as
Claude Code already send `Accept: text/markdown`.

Recommendations:

- `doc/worker.js` already fronts the assets for redirects; add the `Accept`
  check there, falling through to HTML when a page has no `.md` copy.
- VitePress's `sitemap.hostname` option builds the sitemap.
- The managed preamble is a zone setting; turn it off where the zone is
  declared, or record a `Maintainer:` item if no tofu owns it.

## Related

- [moq-dev/moq.dev: llms.txt and sitemap](https://github.com/moq-dev/moq.dev/tree/main/quest) - the same for moq.dev
