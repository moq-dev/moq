# [M] moq announced follows announcements live

## Goal

`moq ls` becomes `moq announced`, which only follows. On a terminal it
redraws a list of what is announced right now, updating on each start and
end. When piped, or with `--json`, it prints one `+`/`-` line per event, as
`moq ls --follow` does today. `moq ls` and its one-shot mode are gone. Shell
completion is local-only: flags, subcommands, and local capture devices;
nothing dials a relay or reads a catalog. Nothing in `rs/moq-cli` reads the
announce `Live` marker afterwards.

## Plan

Decided 2026-09-29 by the maintainer:

- Follow-only. A one-shot listing needs to know when the initial set is
  complete, which is the only thing the `Live` marker is used for. No
  customer needs a one-shot listing, so it waits until one does. Why:
  [Delete the live marker](/quest/m1/announce-live-removal.md) removes that
  marker because an origin fed by many sessions can't answer "caught up"
  honestly.
- The name says what it shows: announcements, as they happen.
- No `ls` alias: it becomes an unknown command, per the no-compat-shim rule.
- No network completion. Shell completion (`rs/moq-cli/src/complete.rs`)
  drops the `BROADCAST` completer, which waits on `Live` today and would
  otherwise need a timeout, and the catalog rendition completers
  (`--video-name`, `--audio-name`), along with whatever exists only to let a
  completer dial. Why: completion stays fast and works offline. Local
  capture-device completers stay.
- The live view is plain terminal redraw. Prefer a maintained crate if the
  redraw grows beyond a few lines. Keep the event-line output byte-for-byte
  compatible with today's `--follow` output so scripts only change the
  command name.

Update every invocation: `doc/bin/cli.md`, `doc/bin/inspect.md`,
`doc/bin/relay/http.md`, `doc/concept/moq-lite.md`, demo recipes, and
anything else a repo-wide search for `moq ls` finds. Check them against
`--help`.

Public API: moq-cli command rename, which is breaking for scripts. Wire: none.
