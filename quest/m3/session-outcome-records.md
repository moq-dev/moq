# [M] Relay stats publish session outcome records

## Goal

Beside the cumulative `sessions_refused` and abnormal-end counters
(`rs/moq-net/src/stats/outcomes.rs`), a relay's stats publish one record per
outcome as it happens, under the session's stats label: a refused session
(when, `RefusalKind`, requested path, transport) and an abnormally ended
admitted session (when, its abnormal-end kind, path, transport, duration). A
subscriber to a label's records sees each one live, so an operator can
answer "why was this connection refused" or "why did this session drop"
for one event, which the counters cannot.

Non-goals: client addresses or any other personal data in a record,
retention beyond the track's cache, and errors within a healthy session.

## Plan

Decided 2026-11-04 (maintainer, planning a live per-project tail of these
records):

- What a record carries:
  - ✅ Refusals and abnormal ends, with no client address. Why: covers both
    "can't connect" and "dropped", and adds no personal data beyond what
    stats already expose.
  - Refusals only.
  - Refusals, ends, and the client address. Rejected: personal data on a
    track any label reader sees.

- The records are a per-label track beside `<label>/presence.json`, named by a
  helper next to `presence_track` in `rs/moq-stats` and produced and consumed
  there, documented in `doc/concept/stats.md`. Every stats track today is a
  compressed `moq_json::snapshot`, which suits state rather than a log of
  events: pick `stream` or `window`, say in the PR why it departs from
  `snapshot`, and update the doc's Encoding section, which says every stats
  track is a snapshot.
- Bounded by design. A peer can cause unlimited refusals, so records past a
  per-label rate are not published one by one: the track publishes a summary
  (count per kind over the interval) instead, and the cumulative counters stay
  exact. Measure the bytes a refusal flood costs with and without the bound.
- Refusal records go through `moq_net::stats::Handle::refuse`, so a gateway's
  refusals appear the same way as native ones. It takes a `#[non_exhaustive]`
  `stats::Refusal`, which carries only the label and kind today, and the
  relay's `Refusals::record`/`record_auth` (`rs/moq-relay/src/refusals.rs`)
  pass only a kind or the refusal's tier and root, so the record's path and
  transport have no way in. Add the requested path and transport as fields of
  `Refusal`, and have the relay pass them from the `moq_auth::Request` it
  holds at every refusal site. A field is optional only where a real caller
  lacks it.
- Abnormal-end records are emitted in `moq-net` where `sessions_failed` is
  bumped today (`SessionInner::drop`, with the kind `Session::end` set), not
  through `refuse`. The stats session therefore learns its path, transport,
  and start time when created, so they become fields of the
  `#[non_exhaustive]` `stats::SessionInfo` that `Handle::session` takes (the
  relay knows `request.transport()` at admission).
- Rust and JS readers parse the track.

Tests: a refused session and an abnormal end each produce exactly one record
under the right label; a flood past the rate produces summaries whose counts
match the counters; an admitted session that ends normally produces none.

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [moq-dev/moq#8266](https://github.com/moq-dev/moq/pull/8266), since merged, made `stats::Handle::{refuse, session}` take `#[non_exhaustive]` structs, so the records add fields and are not breaking.
