# [M] Session outcomes

## Goal

The relay's session stats carry, per tier in the `totals.json` track and per
root in the requested `<root>/presence.json` track
([Stats](/doc/concept/stats.md#tracks)), how many sessions were
refused, by reason, and how many ended abnormally, by kind: idle timeout,
transport error, application error, or token expired. A dashboard computes an
error rate from the stats feed alone, with no log scraping. Refusals count
unattributed, on the default tier, until
[Typed refusal reason](/quest/m1/refusal-reason.md) adds `expired` and
attributes a refusal to the root and tier the auth server resolved.

An embedding application records a refusal on its stats handle, with the
same reasons and its own root and tier, for a connection it refuses outside
the relay's admission (a gateway protocol it terminates itself). A refusal
through `Cluster::admit` counts like any other.

## Plan

Decided 2026-10-07, for per-project error counts downstream (browser
playback needs an error rate it cannot see client-side):

- **Refusals reuse moq#4825's reasons** (`refusals.rs`: refused, unavailable,
  request, forbidden, lan), so `/metrics` and the stats track share one
  vocabulary.
- **The relay's refusals stay unattributed here** (decided 2026-10-08). The
  relay verifies no token itself (its deciders are the auth server, public rules, or
  refuse-all in `rs/moq-relay/src/auth.rs`), and a refused session has no
  grant, so no root or tier. Count a refusal under the empty root on the
  default tier. `group_key` sends the empty root to the root group
  (`<prefix>/node/<node>`) at any `--stats-depth`, so it lands there, created
  on first sight and following the stats linger and epoch; a project's
  group never sees it. Sessions admitted under the empty root (public rules)
  share that group. `expired` and per-root attribution need the decider to
  say so, which [Typed refusal reason](/quest/m1/refusal-reason.md) adds.
- **Embedder refusals** (decided 2026-10-10, maintainer priority). An
  embedder that terminates a gateway protocol itself refuses some
  connections before or without relay admission, and today cannot count
  them: `Refusals` (`rs/moq-relay/src/refusals.rs`) is `pub(crate)`, and
  `Registry::tier(..).session(root)` has no refusal API. Give the stats
  handle one (shaped like `tier(..).refuse(root, reason)`), with the reason
  type public beside `Presence` so both share one taxonomy. The embedder
  names the root and tier; it is trusted to name one it resolved, never a
  dialed path, the same bound as
  [Typed refusal reason](/quest/m1/refusal-reason.md). `/metrics` sees these
  refusals too, ideally by rendering refusals from the registry the way
  `moq_relay_sessions_opened_total` already is, so there is one counter.
- **Gateway admission counts** (folded from the m2 refusal-reasons quest,
  2026-10-10). `Cluster::admit` returns a refusal (from `Auth::admit` or
  `scope`) to the embedder uncounted. Native QUIC (`Connection::run` in
  `connection.rs`) and the non-LAN io_uring branch (`uring.rs`) already record
  whatever `cluster.admit` returns, and WebSocket records around its own
  `scope`. Make `Cluster::admit` the single owner of its refusals, unattributed
  like the relay's own, and drop the QUIC and io_uring records of its errors;
  those callers keep recording only what they refuse themselves (LAN). The
  embedder records through the handle only what the relay never saw.
- **Ends are classified from a typed close kind, not a string.** A peer's
  application close is already typed (`Error::from_transport` decodes it into
  `Error::Session`), but `moq_net::Error::Transport(String)` still flattens
  idle timeout and connection error, and `supervise` in
  `rs/moq-relay/src/connection.rs` reports `err.to_string()`. Add the typed
  kind where the QUIC backend reports the close. Clean covers `Error::Cancel`,
  a peer's zero-code close (`Error::Session(SessionError::Cancel)`), a local
  close (`Error::Closed`), drain, and shutdown; `supervise` matches only
  `Error::Cancel` today, so a classifier built on it would count every normal
  browser close as failed. A lease ending `expired` is the expired kind.
- **Wire shape:** `Presence` gains `sessions_refused` and `sessions_failed`,
  fixed named counters per reason and per kind, so `Presence` stays `Copy`
  (what downstream aggregators plan against). `sessions_ended` stays
  the total, so the live count (`sessions_started - sessions_ended`) is
  unchanged. Readers take them from the totals and presence tracks (quest
  audit, 2026-10-08); `sessions.json` stays for existing readers (#5084, kept
  backwards compatible 2026-10-12) and carries the same `Presence` entries.
- **Outcomes follow the stats counter rules.** A pruned root folds its
  counts into the group totals, and the aggregator merges with a baseline per
  upstream (node, epoch), so a node leaving or returning never reads as new
  errors or a drop.
- Tests: a forbidden path and an auth server refusal each count under their
  reason; an embedder refusal on its handle counts under its root, tier, and
  reason, in the stats track and `/metrics`; a `Cluster::admit` refusal
  counts once, through an embedder, native QUIC, and io_uring alike; a refusal with `--stats-depth 1` and no admitted session
  appears in the root group's totals; a clean close, a peer's zero-code
  close, an idle timeout, and a peer application error each count as their
  kind; the aggregator sums the counters across nodes, and a node departing
  and returning while another stays leaves the merged counts unchanged.

Public API: a refusal reason type and a refusal method on `stats::Handle`;
`Presence` gains the counters. Wire: the stats track's `Presence` fields.

## Related

- [Typed refusal reason](/quest/m1/refusal-reason.md) - adds `expired`
  and per-root, per-tier refusal attribution on top of these counters, and
  the `/metrics` split of an expired token from an invalid one
- [Own the QUIC stack](/quest/m1/quic/README.md) - where a typed close kind
  is natural once `moq-quic` owns the connection
