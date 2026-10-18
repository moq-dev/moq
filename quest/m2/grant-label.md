# [M] Grant stats label

## Goal

An auth grant can carry an optional `label` beside `tier`. A session admitted
under it counts its presence, peaks, HTTP requests, and live sent and
received bytes in stats under `(tier, label)`, while its path scope (what it
may publish and subscribe to) stays the grant's `root`. Absent, the label
defaults to the grant's root, so nothing changes for existing deciders. This
gives live per-credential usage without changing what a session may access.

## Plan

Decided 2026-10-10 by maintainer priority, extended 2026-10-11:

- **Why.** A multi-tenant operator needs each credential's usage while its
  sessions run. Stats key presence by `token.root` (`Cluster::scope` and
  `websocket.rs` call `stats.tier(..).session(&token.root)`), so two
  credentials scoped to the same path are indistinguishable, and bytes are
  keyed only by broadcast path and tier. Per-credential transport bytes reach
  an auth-server decider through `Lease::close`, but only once a session
  ends. `tier` already separates traffic classes; `label` separates who
  within one.
- **Shape.** An optional `label` on `moq_auth::Grant`
  (`rs/moq-auth/src/grant.rs`) and its JS schema (`js/auth/src/contract.ts`),
  carried onto the relay's token and used wherever a session's or request's
  stats context is created.
- **Bound.** It is the auth server's claim, trusted like `tier`. It must be a
  valid path so `--stats-depth` grouping and the per-root track still apply;
  refuse a grant whose label is not one, rather than fall back.
- **Bytes, off the hot path.** Payload bytes stay on the per-`(path, tier)`
  rows as today. Each session also counts its sent and received bytes in its
  own counters, which no other session touches, and a snapshot or report
  folds them into the `(tier, label)` row, with a closed session's totals
  folded in when it drops. A write therefore adds no shared atomic. Reads
  rebuild a row and never consume deltas: the row's finalized total (closed
  sessions and retag checkpoints) plus each live session's counters minus the
  base its last retag recorded. A retag holds the session's lock while it
  loads the counters once, adds them less the base to the old row's
  finalized total, and records them as the new base, and a read takes the
  same lock per session. A concurrent write lands on one side of that single
  load, so it counts exactly once, under the old label or the new. Add a
  write-path benchmark to `rs/moq-net/benches/stats.rs` showing no
  regression against today's per-path bump, and sweep the fold over sessions
  and labels so its cost shows as a slope.
- **Wire.** The sessions output gains sent and received byte fields per
  label. The change is additive: an older reader ignores them and a newer one
  defaults them to zero.
- **HTTP.** A `/fetch` request context counts its bytes under the label too,
  beside its `requests`.
- **Re-checked grants retag live.** A changed or removed label on a re-checked
  grant moves the session to the new label, the way a changed tier does
  today. Generalize `stats::Session::set_tier` into one `(tier, label)` retag
  rather than adding a second method, and call it from `Lease::ended`
  (`rs/moq-relay/src/auth.rs`). Bytes counted before the change stay under
  the old label; bytes after go to the new one, including writes through a
  meter handed out before the retag. A meter therefore carries the session's
  own counters, never a label: the retag folds the session's totals so far
  into the old `(tier, label)` row, and later writes accrue toward the new
  one. The per-path rows keep today's rule, where a meter stays on the tier
  its group started under.
- **Defaults on both sides.** An older relay ignores the field (`Grant` does
  not deny unknown fields) and counts under the grant's root; access is
  unchanged, so that degrades to today's accounting, not to a wider scope.
  Say so in the docs.
- Tests: `label` round-trips in Rust and JS; absent, stats key by the grant's
  root; present, an admitted session's presence and bytes land under the
  label while a publish or subscribe outside the grant's root is still
  refused; a re-checked grant with a new label moves later presence and bytes
  there and leaves earlier bytes under the old one, mirroring
  `set_tier_moves_presence` and `set_tier_moves_subsequent_traffic`
  (`rs/moq-net/src/stats.rs`), and a meter retained across the retag writes
  under the new label; an invalid label is refused.
- Update `doc/concept/stats.md`, `doc/bin/relay/auth.md`,
  `doc/lib/rs/moq-auth.md`, and `doc/lib/js/auth.md`.

Public API: a new optional `Grant::label` in `moq-auth` and `@moq/auth`, and
`stats::Session::set_tier` becomes a `(tier, label)` retag. Wire: the auth
HTTP grant body and the stats track's per-label byte fields, not MoQ.

The per-root session outcome counters follow the same label.

## Related

- [Typed refusal reason](/quest/m1/refusal-reason.md) - a refusal body names
  a root too; decide there whether it may name a label
