# [M] Session outcomes

## Goal

The relay's `sessions.json` stats track carries, per root and tier, how many
sessions were refused, by reason, and how many ended abnormally, by kind:
idle timeout, transport error, application error, or token expired. A
dashboard computes an error rate from the stats feed alone, with no log
scraping. An embedder that decides admission (a CDN's auth layer) can count a
refusal against the root it resolved, since a refused session never gets a
stats context of its own.

## Plan

Decided 2026-10-07, for moq.pro's per-project error counts (a customer
rolling out browser playback wants an error rate it cannot see client-side):

- **Refusals reuse moq#4825's reasons** (`refusals.rs`: refused, unavailable,
  request, forbidden, lan) plus `expired`, so `/metrics` and the stats track
  share one vocabulary. `expired` comes from moq-auth's typed `TokenExpired`;
  [Expired token error](/quest/m1/auth/expired-error.md) is the wire half of
  the same distinction.
- **A refusal counts against a root only when the decider names one.** The
  relay's own token check knows the claimed root only after verifying it, so
  an unverifiable token stays node-wide in `/metrics`. Add a way for a decider's
  refusal to carry the root it resolved, and count it in that root's
  `Presence` on the session's tier.
- **Ends are classified from a typed close kind, not a string.** Today
  `moq_net::Error::Transport(String)` flattens idle timeout, peer
  application close, and connection error, and `supervise` in
  `rs/moq-relay/src/connection.rs` reports `err.to_string()`. Add the
  typed kind where the QUIC backend reports the close. Clean covers
  `Error::Cancel`, a zero application code, drain, and shutdown; a lease
  ending `expired` is the expired kind.
- **Wire shape:** `Presence` gains `sessions_refused` and `sessions_failed`,
  each a map from reason or kind to a cumulative count. `sessions_ended` stays
  the total, so the live count (`sessions_started - sessions_ended`) is
  unchanged. Old readers ignore the new fields; the aggregator sums them like
  the existing counters, keyed by reason.
- Tests: an expired token, a forbidden path, and an embedder refusal each
  count under the right root and reason; a clean close, an idle timeout, and a
  peer application error each count as their kind; the aggregator sums both
  maps across nodes.

## Related

- [WebTransport transport](/quest/m1/auth/webtransport-transport.md) - the
  same consumer's per-transport session split
- [Own the QUIC stack](/quest/m1/quic/README.md) - where a typed close kind
  is natural once `moq-quic` owns the connection
