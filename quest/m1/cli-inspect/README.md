# [S] CLI inspection

## Goal

`moq ls` answers "what is live on this relay" and `moq fetch` reads one group
of a track, over MoQ with the session's own auth, instead of a separate `curl`
to the relay's HTTP `/announced` and `/fetch` endpoints. A guide shows an
operator how to inspect a relay: what is live, how to watch it change, how to
read a group, and where the stats live.

Non-goals: listing tracks or catalogs, showing routes, hops, or sources, and
changing the relay's HTTP endpoints.

## Plan

This README owns the guide, written once both verbs land: a new
`doc/bin/inspect.md` covering `moq ls` and `--follow`, `moq fetch`, their
`curl` equivalents, and reading the relay's stats track, linked from `doc/bin/cli.md`,
`doc/bin/relay/http.md`, and the site sidebar.
