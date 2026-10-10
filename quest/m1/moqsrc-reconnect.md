# [S] moqsrc survives losing its relay connection

## Goal

`moqsrc` survives losing its relay connection: it redials with moq-tokio's
reconnect loop, keeps its pads, and resumes on the next `Start`, as it
already does for a publisher restart. Only a refusal (`NotFound`,
`Unauthorized`) is fatal.

## Plan

Planned 2026-10-10 as a follow-up of #5191, which made `moqsrc` follow its
path's announcements (`origin::Consumer::follow`) and hold its pads from
`End` to `Start`, but kept its one-shot dial: losing the relay connection is
still a session error. Start after #5191 merges.

- Dial through moq-tokio's reconnecting client rather than a single connect,
  so a dropped connection redials and announcements resume. While
  disconnected, pads are held exactly as after an `End`.
- Retry without limit (`backoff.timeout = 0`), as `moqsink` does: the default
  10s budget would turn a longer outage into a bus error.
- Refusals stay fatal, as #5191 decided for the catalog. They surface in two
  places: an `Unauthorized` or `Forbidden` dial ends the reconnect loop
  (`is_auth`), and a `NotFound` path or catalog arrives on a request after a
  successful redial. Both error on the bus instead of redialing forever.
- Test end to end through a loopback relay: kill the relay mid-playback, keep
  it down past the default 10s budget, restart it, and require frames on the
  same pad by name with no bus error. Publish with `moqsink`, which reconnects
  on its own, so the test exercises `moqsrc` resuming rather than a publisher
  that never came back.

Public API: behavior only (no new property). Wire: none.
