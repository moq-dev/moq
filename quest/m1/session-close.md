# [M] Graceful session close withdraws announces

## Goal

Ending a session on purpose withdraws the namespaces it published and waits
for the peer to acknowledge that, up to one second. `abort`, and dropping the
last handle, still end the session immediately and do not wait.

## Plan

Rust has `abort` and drop, and both end the session now. Drop sends a bare
`Cancel`, so `PUBLISH_NAMESPACE_DONE` never goes out.
`moq_net::Session::close()` and `moq_tokio::Connection::close()` already
exist: they wait up to `CLOSE_TIMEOUT` (one second, in `moq-net`'s
`session.rs`) for finished tracks to deliver, then end the session, returning
`Error::Timeout` if the peer was too slow. Extend that drain phase to
unannounce what this session published and wait for the acknowledgements,
under the same deadline. Only moq-lite drains today; IETF sessions close at
once.

JS `Established.close()` is synchronous today. It becomes the same graceful
end and returns a promise, which is a published break, so that change targets
`dev`. `abort` stays immediate in both languages.

`doc/concept/moq-lite.md` says a graceful close withdraws announces and an
abort does not. No new page.

The interop runner is the consumer that found this
([#4209](https://github.com/moq-dev/moq/pull/4209)): after a successful
publish it calls `abort`, so the relay never sees `PUBLISH_NAMESPACE_DONE` and
the next run is told the namespace is already published. Switch it to the
graceful `close()` once that exists. If the calling code lives outside this
repository (moq-interop-runner), that change is a PR there and needs the
maintainer's approval before posting.
