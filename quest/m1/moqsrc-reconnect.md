# [S] moqsrc survives losing its relay connection

## Goal

`moqsrc` survives losing its relay connection: it redials with moq-tokio's
reconnect loop, keeps its pads, and resumes on the next `Start`, as it
already does for a publisher restart. Only a refusal or malformed input is
fatal: a settled CONNECT status (such as `Unauthorized` or `Forbidden`), a
`NotFound` path or catalog, a protocol violation, or a bad catalog.

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
  places: the reconnect loop already ends on an auth failure or any CONNECT
  status other than 408/429/502/503/504 (`status_retryable`), and a `NotFound`
  path or catalog arrives on a request after a successful redial. Both error
  on the bus instead of redialing forever.
- Malformed input (a protocol violation or bad catalog) stays fatal too. The
  reconnect loop retries every established-session error except auth, so the
  implementation must tell a protocol violation from a dropped connection.
- Test end to end through a loopback relay: kill the relay mid-playback, keep
  it down past the default 10s budget, restart it, and require frames on the
  same pad by name with no bus error. Publish with `moqsink`, which reconnects
  on its own, so the test exercises `moqsrc` resuming rather than a publisher
  that never came back. Two more cases require a terminal bus error after
  playback began: a refused redial (for example a 403), and a session that
  ends in a protocol violation.
- Update `doc/bin/gstreamer.md` so the `moqsrc` lifecycle covers relay loss.

Public API: behavior only (no new property). Wire: none.
