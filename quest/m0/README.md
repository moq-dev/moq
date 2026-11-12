# m0: immediate priorities

## Goal

The work in flight now: passing Nightly, Interop, and Video runs, backports
and patch releases for the `release` line, CI cost and latency, test flakes,
and a few small fixes for hostile input, WebSocket throughput, agent disk use,
and CRLF checkouts. The quest merge driver
([moq-dev/moq#6728](https://github.com/moq-dev/moq/pull/6728)), local reviews
([moq-dev/moq#7175](https://github.com/moq-dev/moq/pull/7175)), and the
declared HLS segment grid
([moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652)) landed.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done.

Per-session request caps landed in #4820, idle fronts in #5054, and the
lost demand-poll wake in #5091.

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.
