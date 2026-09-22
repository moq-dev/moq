# `moq ls`

## Goal

`moq --connect <url> ls [prefix]` answers "what is live on this relay" over
MoQ, with the session's own auth, instead of a separate `curl` to the relay's
HTTP `/announced` endpoint. A guide shows an operator how to inspect a relay:
what is live, how to watch it change, and where the stats live.

Non-goals: listing tracks or catalogs, showing routes, hops, or sources, and
changing the relay's `/announced` endpoint.

## Plan

This README owns the guide, written once the verb lands: a new
`doc/bin/inspect.md` covering `moq ls` and `--follow`, `curl /announced`, and
reading the relay's stats track, linked from `doc/bin/cli.md`,
`doc/bin/relay/http.md`, and the site sidebar.

## Quests

- [Caught up](/quest/next/cli-ls/caught-up.md) - moq-net's announce consumer says when the initial set has landed, and shell completion drops its settle timer
- [Verb](/quest/next/cli-ls/verb.md) - `moq ls` prints the live set and exits, or follows changes, as paths or JSON lines
