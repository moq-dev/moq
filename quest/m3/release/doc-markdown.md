# [XS] doc.moq.dev's robots.txt carries no managed preamble

## Goal

`https://doc.moq.dev/robots.txt` is exactly the file `doc/public/robots.txt`:
it names the sitemap and says `Content-Signal: search=yes, ai-input=yes,
ai-train=yes`, with no Cloudflare "managed robots.txt" preamble above it, which
declares a conflicting signal.

## Plan

The worker, sitemap, and robots.txt shipped on `main` in
[moq-dev/moq#9375](https://github.com/moq-dev/moq/pull/9375), but Workers
Builds deploys `moq-doc` from `release`, which lacks them, so the origin
answers `/robots.txt` with a 404.
Checked 2026-11-22: the zone's managed robots.txt (AI Crawl Control → Signals,
"Enable Bot Preference Sync") is already off, so the preamble is what
Cloudflare serves a Free-plan domain that has no robots.txt of its own, and it
[serves an origin's file unchanged](https://developers.cloudflare.com/bots/additional-configurations/managed-robots-txt/).
No zone setting changes.

## Waiting

- moq-dev/moq: the next release cut deploys `doc/`. Check with
  `curl https://doc.moq.dev/robots.txt`: the first line is `User-agent: *`
  from the repository's file, not a Cloudflare comment.

## Required

- [Release cut](/quest/m3/release/cut.md) - deploys `doc/` from `release`

## Related

- [moq-dev/moq.dev: llms.txt and sitemap](https://github.com/moq-dev/moq.dev/tree/main/quest) - the same for moq.dev
