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

- The records are a track in the existing stats layout, one per label like
  the per-label totals, documented in `doc/concept/stats.md`. Pick its codec
  from moq-json's producers (stream or window) and say why in the PR.
- Bounded by design. A peer can cause unlimited refusals, so records past a
  per-label rate are not published one by one: the track publishes a summary
  (count per kind over the interval) instead, and the cumulative counters stay
  exact. Measure the bytes a refusal flood costs with and without the bound.
- The same records reach embedders through the outcome API that refusals
  already go through (`Handle::refuse`), so a gateway's refusals appear the
  same way as native ones.
- Rust and JS readers parse the track.

Tests: a refused session and an abnormal end each produce exactly one record
under the right label; a flood past the rate produces summaries whose counts
match the counters; an admitted session that ends normally produces none.
