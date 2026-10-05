# [S] Per-worker transcode epochs

## Goal

Two moq-transcode workers never share a broadcast name. Each worker
(`rs/moq-transcode`, and `moq transcode` in moq-cli) publishes its output under
an epoch it mints, so two encoders' different bytes are two broadcasts. A
viewer of the derived name follows the newest worker's epoch, like any other
epoch takeover, and a relay never splices one worker's group into another's.

Non-goals: byte-identical output across workers, and a deterministic catalog
(stable rung names, a pinned encoder kind). Neither is needed once each worker
is its own broadcast.

## Plan

Decided 2026-10-05 by the maintainer:

- The splice is closed by per-worker names. Two workers that double-claim one
  derived path under the [wildcard](/quest/m0/wildcard/README.md) line encode
  the same source group into different bytes. If they shared a name, a
  #4741 route move would make the relay continue group N at frame M from the
  other worker's copy (`Recover::poll_serving` in
  `rs/moq-net/src/model/resume.rs` subscribes the new copy from frame 0 and
  splices with `start_at(M)` itself), which no moq-transcode policy can
  refuse. A name reused for different content is a bug (AGENTS.md), so each
  worker gets its own.
- Rejected: a moq-net `track::Info::whole_groups` serving policy (it never
  fires on the relay's own splice), resuming only at group boundaries, and
  byte-deterministic workers.
- Layout: the derived name mirrors the source's bare name, and the worker
  appends an epoch it mints: `.pro/transcode/<pid>/foo.hang/@<worker>`. The
  source's own epoch does not nest beneath it, because
  [Origin](/quest/m0/broadcast-epoch/origin.md) treats a path whose final
  segment is an epoch as pinned, so `foo.hang/@e/@w` could not be followed.
  The worker's catalog references the exact source epoch it transcodes, so
  one derived broadcast is self-consistent, and a source restart is
  transcoded under a newer worker epoch. The default CLI output
  (`<source>/transcode.hang`) gains the same trailing epoch.
- A caller may pass an explicit epoch; otherwise the worker mints one. The
  epoch segment adds a level to the catalog's relative source reference
  (`Config::source`).

Done in [#4812](https://github.com/moq-dev/moq/pull/4812): the rung's fetch
handler refuses a mid-group FETCH instead of writing the whole group at the
requested index, and a test pins two instances fed one source publishing the
same catalog and group sequences.

Left: mint and append the worker epoch in moq-transcode or the CLI, fix the
catalog's source reference, and test that two workers land on distinct paths
and a viewer of the derived name follows the newer one. Update
`doc/bin/cli.md` and `doc/bin/obs.md`, which give the output path.

Public API: possibly an epoch on the transcode output config. Wire: none.

## Required

- [Origin](/quest/m0/broadcast-epoch/origin.md) - viewers of a bare name follow its newest live epoch, which is how a derived-name viewer finds the current worker

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - a double claim's two workers are two broadcasts, so a relay never splices them
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - the epoch rules the worker's own epoch follows
