# [L] Client goaway

## Goal

The JavaScript client migrates on GOAWAY the way the Rust client already
does: it dials the replacement while the old session keeps serving, follows a
redirect URI under the same guard, and keeps the app-visible handle and its
origins across the swap. The Rust client's fleet-drain path (an empty-URI
GOAWAY redialed through a fresh DNS resolve) is pinned by a regression test.

## Plan

moq.pro's (downstream) fleet drain orchestration relies on this behavior: a
drained node is already out of DNS when GOAWAY fires, so a re-resolve is what
lands clients on a healthy relay.

### Rust is done except the proof

`moq_tokio::Connection` already handles GOAWAY: the session loop returns the
message, `Redirect::resolve` guards the URI (scheme tier never drops, no
widening to a local host, optional same-host mode), the loop redials while a
`Draining` handle keeps the old session serving until it closes or overstays
the handover cap, and `Status::Migrating` is visible to callers. Every dial
resolves DNS again, since `Addrs` holds URLs and each backend resolves at dial
time, so nothing is pinned. The relay always sends an empty URI, and the only
end-to-end coverage is the cluster sibling test with a redirect.

Add the fleet-drain regression test: a relay GOAWAYs with an empty URI and a
timeout, the client redials the configured URL through a fresh resolve (a
resolver the test can repoint), live tracks hand over at a group boundary,
and the old session closes within the handover cap. The name-based redirect
guard stays [its own quest](/quest/m1/2624-moq-native-goaway-redirect-guard-classifies-hosts-by-name.md).

### JavaScript is greenfield

Today `js/net` logs the lite GOAWAY URI, logs the IETF draft-17+ URI, and does
not decode the body at all on the draft-14 to -16 shared control stream.
`Reload` reconnects only on `closed`, tears the old connection down in its
effect cleanup, and `Connection.Shared` pools by URL href.

- Surface the peer's GOAWAY on `Established` as a drain signal carrying the
  resolved URI and the timeout, decoded on every wire the client speaks,
  including the draft-14 to -16 adapter route that currently returns before
  reading the body.
- `Reload` mirrors `Draining`: on GOAWAY it dials the target immediately,
  swaps the origin wiring (`forwardAnnounced`, `publish`, `subscribe`) once
  the replacement is established, and leaves the old session to close on its
  own or at a handover cap, `min(peer timeout, configured cap)`. Groups in
  flight finish. A GOAWAY does not go through the backoff delay; a failed
  replacement dial does.
- Port the guard: follow by default, refuse a scheme-tier drop or a widening
  to a local host, and offer the same-host mode. An empty URI redials the
  current URL. A redirect with a certificate pin (`serverCertificateHashes`)
  is refused unless the host is unchanged, since the pin cannot verify another
  relay; the pool already refuses to share pinned connections.
- `Connection.Shared` re-keys its entry to the redirect target, so a later
  caller configured with that URL shares the migrated connection. The app's
  handle and shared origin are unchanged; only the pool key moves, and a
  caller still asking for the original URL gets a fresh entry.
- Tests against the in-tree relay: an empty-URI drain migrates without a
  dropped group, a redirect moves the pool key and origins, each guard refusal
  closes rather than reconnects, and the draft-14 to -16 route decodes the
  URI.

## Related

- [Redirect guard by name](/quest/m1/2624-moq-native-goaway-redirect-guard-classifies-hosts-by-name.md) - the Rust guard classifies local hosts by name; the JS port inherits the same gap until it lands
