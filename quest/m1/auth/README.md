# Auth server

## Goal

One way to authorize a connection, on moq-relay and on `moq --server-bind`
alike. An operator either points `--auth-url` at an auth server that answers
one JSON request per connection, or grants anonymous access with
`--auth-public`, never both, and nothing else admits anyone. The `moq-auth`
crate and the `@moq/auth` package own that contract and the JWT that rides it,
retiring `moq-token`. `moq auth serve` is the reference server carrying the
policy the relay used to hold: signing keys, public rules, an explicit grant
for mTLS peers, and session limits. A verified client certificate is a fact in
the request, never a grant by itself, so the unrestricted mTLS default is gone.

Today the relay has three ways in that are easy to confuse: a scoped JWT it
verifies itself, an mTLS peer admitted unscoped to whatever path it dials
(#3603), and `--auth-api` in token or proxy mode with a cache keyed on
`Cache-Control` directives an operator has to know about. This line replaces
all three with one request, one reply, and one place to put policy. The old
[Auth API](https://github.com/moq-dev/moq/issues/3087) questline that patched
the endpoint contract in place is deleted; its issues close here.

Boundaries: the wire is untouched. In-band tokens keep their own line under
[In-band auth](/quest/m2/auth/README.md) and reuse this contract. Client-side
challenges are out of scope; a peer that must prove itself at the transport
uses mTLS. The cluster keeps dialing with mTLS or a URL token; only the
accepting side changes.

## Plan

Decisions settled while planning, recorded so review does not relitigate them.

### The contract

- **One JSON POST per event.** The body is a request; the reply is a grant.
  Three events share one shape: `connect` when a session is accepted,
  `revalidate` when the grant asked to be re-checked, and `end` when the
  session closes. Every field the relay knows is sent; nothing is parsed on
  the relay's behalf, so a server keys policy on the raw path and query and no
  JWT parameter is special.
- **Request:** `id` (random 128-bit, hex, unique per session), `event`,
  `node` (the operator's name for this relay, from the existing node setting),
  `transport` (`quic`, `websocket`, `tcp`, `unix`, `iroh`), `remote` and
  `local` socket addresses, `server_name` (SNI) and `alpn` (the negotiated
  protocol, including the moq version), `path` exactly as dialed, `query`
  raw, `role` the client declared at SETUP, and `tls` with the verified client
  certificate when one was presented: `name` (first SAN DNS name, else CN,
  else the fingerprint so it is never empty), `fingerprint` (SHA-256 of the
  leaf), `expires` (notAfter), `issuer`. An `end` event adds `reason`,
  `duration`, and `bytes` sent and received.
- **Reply:** `publish` and `subscribe` as `moq-pattern` unions (`foo/**` is a
  subtree, `**` is everything, an empty list is nothing), `root` (optional;
  the path the grant is relative to, replacing the dialed path, which is how
  an operator aliases a slug to a canonical id), `expires` (optional unix
  seconds; the session closes then), `revalidate` (optional seconds until the
  relay asks again), and `tier` (optional opaque label handed to stats). A
  2xx with a grant admits. A 403 refuses. Any other outcome at connect, a
  timeout, a 5xx, or an unparseable body, refuses and logs an error; nothing
  is admitted because the server was down. A grant that names nothing
  refuses. There is no version field: the schema is the crate's version.
- **Revalidate and outage.** A re-check that returns a grant applies it: a
  changed `tier` retags subsequent bytes, a changed `root` closes the session,
  a narrower grant closes the session until
  [Origin scopes](/quest/m2/path-patterns/origin.md) resizes in place. A
  refusal closes it now. A failed re-check retries with backoff and the
  session lives until `expires`; a grant with `revalidate` and no `expires`
  is refused at startup validation of the reply, so an outage always has a
  bound the server chose. No `Cache-Control`, `max-age`, `stale-if-error`, or
  `stale-while-revalidate` anywhere.
- **Patterns only.** Grants and JWT claims use `moq-pattern` from day one.
  There is no prefix mode and no `v` field. The relay enforces prefix-shaped
  patterns first (a literal, `literal/**`, or bare `**`, which is the empty
  prefix) and refuses any other pattern at connect, naming it; Origin scopes lifts that restriction with no contract
  change.

### Composition

- **A lease, not a trait.** `moq_auth::lease::{Producer, Consumer}`, like
  every moq-net handle. Whoever runs the accept loop turns the transport's
  request into a `moq_auth::Request`, gets a `Consumer` back, and hands it to
  the session; the session reads the current grant and awaits a change or a
  revocation. `moq_auth::Client` is the HTTP implementation: it drives the
  `Producer` (revalidate on cadence, `end` on drop). An embedder that decides
  in-process builds its own `Producer` from any logic and never dials. The
  relay core does no auth and holds no HTTP client.
- **The link is the URL.** `--auth-url` takes `http://` for loopback
  addresses only, `https://` presenting the relay's existing `connect.tls`
  client certificate when one is configured, and `unix://` for a socket. No
  shared secret; a remote server that must know which relay is asking uses
  mTLS.
- **Static public is the only other way in.** `--auth-public` (with its
  `-subscribe` and `-publish` forms) grants anonymous connections a pattern
  union and refuses everything else. Setting it with `--auth-url` is a
  startup error. Every other auth flag is deleted: `--auth-api`,
  `--auth-api-mode`, `--auth-key`, `--auth-key-dir`, `--auth-public-api`,
  `--auth-domain`, `--auth-mtls-tier`, and the hidden `--auth-tls-*` family.
  A relay with neither flag refuses to start, and `listen.tls.root` alone no
  longer counts as "someone can authenticate".
- **mTLS is a fact.** `listen.tls.root` still verifies the chain at the
  handshake and a bad chain still fails there. The identity goes in the
  request and the server decides; `moq auth serve` grants it only what
  `--mtls-publish` and `--mtls-subscribe` name, empty by default. Cluster
  peers are admitted the same way: moq.pro's server grants its cluster CA
  everything. The mDNS LAN credential stays relay-internal, since it is a
  secret the relay minted for itself, not a peer identity a server knows.
- **moq-token retires into moq-auth.** Claims become pattern unions; keys,
  signing, and verification move over; `moq auth generate|sign|verify`
  replaces `moq token` and the standalone binary, the end state #3046
  (closed as not planned) weighed.

### Downstream

moq.pro implements the lease in-process inside its edge binary, keeps calling
its Worker per signing key so an audience costs one request per relay, grants
its cluster CA everything locally, and translates put/get prefix claims for a
deprecation window. Its quests live in that repository's tree.

### Order

The package landed first, because the server and the relay both build on it
(`rs/moq-auth`, `js/auth`, `moq auth`); the server second
(`moq_auth::serve`, `moq auth serve`), because the relay's tests run against
it; the relay last, the largest change and the one that deletes the old path. This line gates
[Merge dev](/quest/m1/merge-dev.md): it is a breaking change to flags, claims,
and package names, and the release moq.pro adopts must carry it.

## Quests

- [Relay](/quest/m1/auth/relay.md) - moq-relay and `moq --server-bind` admit
  through a lease, `--auth-url` or `--auth-public` is the whole configuration,
  and nothing is unrestricted

## Related

- [In-band auth](/quest/m2/auth/README.md) - a token presented after the
  connection becomes a request through the same contract
- [Origin scopes](/quest/m2/path-patterns/origin.md) - lifts the prefix-shaped
  restriction and resizes a live session on a narrower grant
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - the client-side
  handling of a refusal, unchanged by this line
