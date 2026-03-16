# Interoperability
Want to test some standards against this code?

It's recommended that you run the code locally for easier debugging. See the [Setup Guide](/setup/dev), it's pretty easy.

## Transport
[moq-lite](/concept/layer/moq-lite) is a forwards-compatible subset of moq-transport.
A moq-lite client can talk to any moq-transport relay, but not necessarily the other way around.

### Versions
- moq-transport-14
- moq-transport-15 (untested)
- moq-transport-16
- moq-transport-17

### Protocols
- WebTransport
- QMux over WebSocket
- (Rust) QUIC
- (Rust) QMux over TLS

### Client
If you want to test your relay:

```bash
# Rust: Publishes a media broadcast
just pub bbb https://<your relay>

# Javascript: Subscribes to a media broadcast
just web https://<your relay>
```

> **Note:** WebTransport automatically prepends the URL path (e.g., `/anon`) to broadcast names. Raw QUIC and iroh have no HTTP layer, so you must manually include that prefix in the broadcast name (e.g., publish as `/anon/bbb`).

The publisher sends a `PUBLISH_NAMESPACE` and the subscriber **requires** a `SUBSCRIBE_NAMESPACE`.
If you haven't implemented the latter yet, remove `reload` in `js/demo/src/index.html`.

**Feeling spicy?**
You can use [gstreamer](/app/gstreamer) or [obs](/app/obs) too, both support publishing and subscribing.
There's also bindings for Rust, Javascript, C, Python, Kotlin, Swift, etc.

### Relay
The relay is less useful to test against because we purposely implement a shallow subset of the moq-transport draft.

```bash
# Rust: Runs a localhost relay
# Also available at: https://cdn.moq.dev/anon
just relay
```

The relay **WILL IGNORE** the following:
- Any sub-group >0
- Any datagrams
- Any FETCH, except JOINING FETCH is a no-op.
- Any objects with delta >0 (must be contiguous)
- Any object properties
- Any SUBSCRIBE `forward=0`
- Any multi-publisher nonsense
- Probably some other stuff

Note that all subscriptions start at the latest group.


## Media
I'm primarily using `hang`, which is like a mix between MSF + LOC.
I'd love to standardize eventually but I need more media homies that want to interop, not just write a spec.

### Rust
The publisher currently makes two catalog tracks pointing to the same media tracks:
- `catalog.json` ([hang](/concept/layer/hang))
- `catalog` ([MSF](/concept/standard/msf))

We support two possible containers, but currently `cmaf` is experimental:
- `legacy` ([hang](/concept/layer/hang))
- `cmaf` ([CMAF](/concept/standard/cmaf))

### Javascript
The Javascript code currently only supports `catalog.json`.
However, the subscriber can consume both `legacy` and `cmaf` containers
