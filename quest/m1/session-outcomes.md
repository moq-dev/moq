# [M] Session outcomes

## Goal

The relay's session stats carry, per tier in [stats-split](/quest/m0/broadcast-epoch/stats-split.md)'s
totals and per root in its requested per-root track, how many sessions were
refused, by reason, and how many ended abnormally, by kind: idle timeout,
transport error, application error, or token expired. A dashboard computes an
error rate from the stats feed alone, with no log scraping. Refusals count
unattributed, on the default tier, until
[Typed refusal reason](/quest/m1/auth/refusal-reason.md) adds `expired` and
attributes a refusal to the root and tier the auth server resolved.

## Plan

Decided 2026-10-07, for moq.pro's per-project error counts (a customer
rolling out browser playback wants an error rate it cannot see client-side):

- **Refusals reuse moq#4825's reasons** (`refusals.rs`: refused, unavailable,
  request, forbidden, lan), so `/metrics` and the stats track share one
  vocabulary.
- **Refusals stay unattributed here** (decided 2026-10-08). The relay verifies
  no token itself (its deciders are the auth server, public rules, or
  refuse-all in `rs/moq-relay/src/auth.rs`), and a refused session has no
  grant, so no root or tier. Count refusals in the default tier's totals;
  `expired` and per-root attribution need the auth server to say so, which
  [Typed refusal reason](/quest/m1/auth/refusal-reason.md) adds.
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
  each a map from reason or kind to a cumulative count. `sessions_ended` stays
  the total, so the live count (`sessions_started - sessions_ended`) is
  unchanged. They ride stats-split's totals and per-root track, not
  `sessions.json`, which stats-split retires (quest audit, 2026-10-08).
  `Presence` is `Copy` today; maps drop that (a moq-net API break to report),
  while a fixed counter per reason and kind keeps it. The PR picks.
- **Outcomes follow stats-split's counter rules.** A pruned root folds its
  counts into the group totals, and the aggregator merges with a baseline per
  upstream (node, epoch), so a node leaving or returning never reads as new
  errors or a drop.
- Tests: a forbidden path and an auth server refusal each count under their
  reason; a clean close, a peer's zero-code close, an idle timeout, and a peer
  application error each count as their kind; the aggregator sums both maps
  across nodes, and a node departing and returning while another stays leaves
  the merged counts unchanged.

## Required

- [Stats totals and prefix tracks](/quest/m0/broadcast-epoch/stats-split.md) -
  the totals and per-root track these counters ride, replacing `sessions.json`

## Related

- [WebTransport transport](/quest/m1/auth/webtransport-transport.md) - the
  same consumer's per-transport session split
- [Typed refusal reason](/quest/m1/auth/refusal-reason.md) - adds `expired`
  and per-root, per-tier refusal attribution on top of these counters
- [Own the QUIC stack](/quest/m1/quic/README.md) - where a typed close kind
  is natural once `moq-quic` owns the connection
