# [S] Cluster idle timeout

## Goal

A relay notices a silent peer relay within seconds, not after the 30 s QUIC
idle timeout it shares with every viewer. moq.pro's routing simulator found
that failure detection, not routing, sets every outage window: a silent link
or relay loss goes unnoticed for the idle timeout in every routing design,
and subscribes through it go nowhere meanwhile.

## Plan

- Cluster sessions get their own idle timeout and keep-alive, shorter than
  the `--quic-*` defaults (`rs/moq-tokio/src/quic.rs`, 30 s idle and 5 s
  keep-alive). A keep-alive alone only keeps a quiet session open; the
  timeout is what detects loss.
- QUIC's effective idle timeout is the smaller of the two endpoints'
  (RFC 9000 section 10.1), so the dialing relay's setting bounds both sides of
  a cluster link. Say so where the setting is documented.
- Pick the default in the PR with its reasoning: loss detection against
  spurious drops on a lossy intercontinental link.
- Docs: `doc/bin/relay/cluster.md` and `doc/bin/relay/config.md`.

Public API: adds a relay config field; lands on `main`.

## Related

- [Cluster routing](/quest/m1/cluster-routing/README.md) - its liveness and failover depend on this detection
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - viewers wait out the same timeout for an old route
