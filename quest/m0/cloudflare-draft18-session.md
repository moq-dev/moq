# [S] A draft-18 session to Cloudflare outlives its first request

## Goal

A moq-transport-18 session from moq-net to Cloudflare's public draft-18 relay
stays up past its first request instead of closing with `protocol violation`.
Done when `moq --connect
https://draft-18.cloudflare.mediaoverquic.com/<random> fetch publisher.json`,
built from `main`, stays connected until its own timeout: a trace shows
Cloudflare answered the first SUBSCRIBE_NAMESPACE and no CONNECTION_CLOSE
arrives before the timeout, so a stalled setup does not count. If the
violation is ours, the fix lands here with a test; if it is Cloudflare's, a
reproducer is reported upstream and this quest waits on that fix.

## Plan

Seen 2026-11-07 from the `moq` CLI on a recent `main` build and from a
moq-net publisher dialing the same relay six times: each session negotiates
`moqt-18`, then ends with `protocol violation` 13 to 120 ms in, right after
the client's first SUBSCRIBE_NAMESPACE. The trace does not say which side
raised it.

The [interop audit](/quest/m3/audit-interop.md) records that `main`
passed against Cloudflare draft-18 after
[moq-dev/moq#5483](https://github.com/moq-dev/moq/pull/5483), but that was
the JavaScript lifecycle test, not this Rust CLI fetch. So this is a
regression since then, a change on Cloudflare's deployed relay, or a Rust
CLI path that never worked.
Find which first: read who sends the CONNECTION_CLOSE and its reason with
`RUST_LOG=trace` (or a qlog). Then build cloudflare/moq-rs's `draft-18-dev`
relay locally (as `test/smoke/cloudflare.sh` does), and run the same
`main`-built `moq --connect ... fetch publisher.json` against it, trusting its
self-signed certificate. A local reproduction can bisect our side; a
remote-only failure points at the deployed relay's version only once the
peer's SETUP and our resulting SUBSCRIBE_NAMESPACE bytes match between the
two runs; differing SETUP parameters can leave a client-side bug untested. `just cloudflare`
stays a separate control: its only client is `clients/cloudflare`, built
against cloudflare/moq-rs, so it never exercises moq-net.

Fix only what this session needs: draft-18 is a compatibility layer, so
ignore requirements that add cost without a behavioral benefit. Filing on
cloudflare/moq-rs is outside the moq-dev org and needs the maintainer's
approval first; search its open issues
([#281](https://github.com/cloudflare/moq-rs/issues/281),
[#282](https://github.com/cloudflare/moq-rs/issues/282)) before filing.
