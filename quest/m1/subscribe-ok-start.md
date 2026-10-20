# [S] SUBSCRIBE_OK names where delivery starts

## Goal

SUBSCRIBE_OK's `Group` is the first group the publisher actually delivers,
as the lite draft defines it, so a warm relay and a cold one answer the same
subscription the same way. Today a publisher whose read cursor gives up
cached groups as stale before the start resolves still names the cache's
oldest group, claiming groups it never sends. Only a group skipped for a
missing head is excluded now.

## Plan

Found while building SUBSCRIBE_ALL
([moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931), commit
`f074d09e7`), which fixed it in `TrackRun` (`rs/moq-net/src/lite/publisher.rs`)
and covered it with `the_start_names_past_the_stale_groups_it_skips`. That
PR is being replaced, so port the fix and its test on their own: record
every range the cursor gives up before the start resolves (a missing head,
stale, or dropped upstream), and resolve the start past them. Check whether
the moq-transport publisher's SUBSCRIBE_OK and the JS publisher have the same
gap, and fix them here if so.

Add a relay test on mock time: the same SUBSCRIBE through a cold relay and a
warm one whose cache holds groups older than the subscriber's max age gets
the same `Group`.

Public API: none. Wire: no encoding change; SUBSCRIBE_OK's `Group` changes
behavior on every lite version with a resolved start (lite-05 on) to match
its definition.

## Related

- [TRACKS](/quest/m1/archive/tracks.md) - the line of work that surfaced it
