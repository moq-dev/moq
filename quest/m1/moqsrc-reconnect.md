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
still a session error.

- Dial through moq-tokio's reconnecting client rather than a single connect,
  so a dropped connection redials and announcements resume. While
  disconnected, pads are held exactly as after an `End`.
- Refusals stay fatal, as #5191 decided for the catalog: a refused connect or
  path errors on the bus instead of redialing forever.
- Test end to end through a loopback relay: kill the relay mid-playback,
  restart it, republish, and require frames on the same pad by name with no
  bus error.

Public API: behavior only (no new property). Wire: none.
