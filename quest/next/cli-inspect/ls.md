# [S] The ls verb lists active announcements

## Goal

`moq --connect <url> ls [prefix]` prints the broadcasts live under `prefix`,
one path per line relative to the connect URL's root, and exits once caught
up. `--follow` first prints the announce stream's initial replay, one `+ path`
per active route, then `+ path` / `- path` as broadcasts come and go. `--json`
prints one `{"path": .., "active": bool}` per line in either mode. Like the
relay's `/announced`, it lists announced prefixes, which by convention are
broadcast paths. An `Updated` event (a new route for a path already live) is
not printed. `--follow` runs until interrupted; if the announce stream ends,
it exits non-zero.

## Plan

- A MoQ verb beside import/export/play, not a stageable one: it consumes the
  origin and never publishes. It is a subscriber-only session, so it works
  with a subscribe-only token.
- Update `doc/bin/cli.md`: add the verb to the table, and point the Debugging
  section at `moq ls` next to `curl /announced`. The verb table still lists
  `token` where the code has `auth`; fix it while there.
- Test: against an in-process relay, snapshot mode prints exactly the
  announced set and exits, `--follow` prints the initial replay then a `-` when
  a publisher leaves, and `--json` lines parse.

## Required

- [Caught up](/quest/next/cli-inspect/caught-up.md) - snapshot mode exits on the consumer's caught-up signal
