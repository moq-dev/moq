# [XS] doc.moq.dev's robots.txt carries no managed preamble

## Goal

`https://doc.moq.dev/robots.txt` is exactly the file `doc/public/robots.txt`:
it names the sitemap and says `Content-Signal: search=yes, ai-input=yes,
ai-train=yes`, with no Cloudflare "managed robots.txt" preamble above it, which
declares a conflicting signal.

## Plan

The worker, sitemap, and robots.txt shipped; the preamble is a zone setting no
file in this repository declares, so an agent cannot change it.

## Waiting

- Maintainer: turn off Cloudflare's managed robots.txt for the zone that
  serves doc.moq.dev (dashboard: AI Crawl Control → Robots.txt, or Security →
  Settings → "Manage your robots.txt"); an agent has no zone token. Check with
  `curl https://doc.moq.dev/robots.txt`: the first line is `User-agent: *` from
  the repository's file, not a Cloudflare comment.

## Related

- [moq-dev/moq.dev: llms.txt and sitemap](https://github.com/moq-dev/moq.dev/tree/main/quest) - the same for moq.dev
