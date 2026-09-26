# [M] Graceful session close withdraws announces

## Goal

Ending a session on purpose withdraws the namespaces it published and waits
for the peer to acknowledge that, up to one second. `abort`, and dropping the
last handle, still end the session immediately and do not wait.

## Plan

Rust has `abort` and drop, and both end the session now. Drop sends a bare
`Cancel`, so `PUBLISH_NAMESPACE_DONE` never goes out. Add
`moq_tokio::Connection::close()`. It unannounces what this session published,
waits up to one second for the acknowledgements, then ends the session. If
the peer has not answered by then, the session still ends and `close()`
returns that timeout. There is no existing request-ack timer to reuse; this
one second is its own constant.

JS `Established.close()` is synchronous today. It becomes the same graceful
end and returns a promise, which is a published break, so that change targets
`dev`. `abort` stays immediate in both languages.

`doc/concept/moq-lite.md` says a graceful close withdraws announces and an
abort does not. No new page.
