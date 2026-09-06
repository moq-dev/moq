# [L] Client goaway

## Goal

The JavaScript client migrates on GOAWAY the way the Rust client already
does: it dials the replacement while the old session keeps serving, follows a
redirect URI under the same guard, and keeps the app-visible handle and its
origins across the swap. The Rust client's fleet-drain path (an empty-URI
GOAWAY redialed through a fresh DNS resolve) gains the regression test it
lacks today.

## Plan

moq.pro's (downstream) fleet drain orchestration relies on this behavior: a
drained node is already out of DNS when GOAWAY fires, so a re-resolve is what
lands clients on a healthy relay.

Everything below describes `dev`, which is where this quest lands: `main`
still has `moq-native`'s close-only `Reconnect` and no `Connection.Shared`.
It lands after [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md)
collapses `Reload` and `Shared` into one `Connection`, so the drain loop is
written once into that class and consumers migrate their call sites once;
where the text below says `Reload` or `Connection.Shared`, read the single
class and its pool.

### Rust is done except the proof

On `dev`, `moq_tokio::Connection` already handles GOAWAY: the session loop returns the
message, `Redirect::resolve` guards the URI (scheme tier never drops, the host
is pinned to the configured one by default, and `follow` is the opt-in that
lets a peer name another), the loop redials while a
`Draining` handle keeps the old session serving until it closes or overstays
the handover cap, and `Status::Migrating` is visible to callers. Every dial
resolves DNS again, since `Addrs` holds URLs and each backend resolves at dial
time, so nothing is pinned. The relay always sends an empty URI, and the only
end-to-end coverage is the cluster sibling test with a redirect.

Add the fleet-drain regression test: a relay GOAWAYs with an empty URI and a
timeout, the client redials the configured URL through a fresh resolve (a
resolver the test can repoint), live tracks hand over at a group boundary,
and the old session closes within the handover cap.

### JavaScript is greenfield

On `dev`, `js/net` logs the lite GOAWAY URI and keeps that session open,
logs the IETF draft-17+ URI and then closes the session when its control
loop ends, and on the draft-14 to -16 shared control stream reads the message
body but returns without decoding it. Nothing migrates: `Reload` reconnects
only after `closed` fires, through its backoff, tears the old connection
down in its effect cleanup, and `Connection.Shared`
(`js/net/src/connection/pool.ts`) pools by URL href.

- Surface the peer's GOAWAY on `Established` as a drain signal carrying the
  resolved URI and the timeout, decoded on every wire the client speaks,
  including the draft-14 to -16 adapter route that currently returns without
  decoding the body it has already read.
- `Reload` mirrors `Draining`: on GOAWAY it dials the target immediately,
  swaps the origin wiring (`forwardAnnounced`, `publish`, `subscribe`) once
  the replacement is established, and leaves the old session to close on its
  own or at a handover cap: the configured cap, lowered to the peer's timeout
  when the wire carried a positive one. Lite and IETF drafts 14 to 16 carry no
  timeout, and the IETF decoder reads an absent one as zero, so absence means
  the cap and never a zero-length handover. Groups in flight finish. A GOAWAY does not go through the backoff delay; a failed
  replacement dial does.
- Port the guard: same-host by default, refuse a scheme-tier drop or a
  widening to a local host, and offer the follow mode. Empty, ignored, malformed, or refused URIs preserve the
  current address list, including caller-selected fallbacks. Only an accepted
  redirect replaces the list. A redirect with a certificate pin (`serverCertificateHashes`)
  is refused unless the host is unchanged, since the pin cannot verify another
  relay; the pool already refuses to share pinned connections.
- `Connection.Shared` re-keys its entry to the redirect target, so a later
  caller configured with that URL shares the migrated connection. The app's
  handle and shared origin are unchanged; only the pool key moves, and a
  caller still asking for the original URL gets a fresh entry. When the
  target key already holds a live entry, that entry wins: the migrating entry
  is removed from the pool without replacing the target entry. Existing
  handles keep its migrated connection, but a new lookup for the original URL
  dials fresh and a target lookup joins the target entry. The migrated entry
  retires when its last existing handle releases it. Two connections to one
  relay for that overlap is the honest cost; entry removal is identity-guarded
  so neither cleanup can delete another entry.
- Tests against the in-tree relay: an empty-URI drain migrates without a
  dropped group, a GOAWAY without a timeout hands over at the configured cap,
  a redirect moves the pool key and origins, a redirect onto an already
  pooled key keeps existing handles on both entries but makes a new caller for
  the original URL dial fresh, each guard refusal closes rather than
  reconnects, and the draft-14 to -16 route decodes the URI.

## Required

- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - one Connection class first, so GOAWAY is built into it rather than into two
