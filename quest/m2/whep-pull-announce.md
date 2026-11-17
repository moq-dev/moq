# [S] A WHEP pull announces its broadcast once the catalog exists

## Goal

`moq import webrtc --connect` announces its broadcast only once the
broadcast has a catalog, so an export in the same process, or a viewer that
arrives first, never resolves the broadcast and then gets `not found` for
its catalog. A WHEP pull chained to another gateway in one process
(`import webrtc --connect <whep> -- export rtmp --connect <rtmp>`) restreams
instead of failing at startup.

## Plan

Found 2026-11-16 while validating the gateway admission change on the built
binary: with both stages starting together, `export rtmp --connect` failed
with `init FLV export: moq: not found`, `export rtmp --listen` refused a
player with `init catalog check: moq: not found`, and `export srt --connect`
ended at once. Each export resolved the broadcast before its catalog track
existed.

The cause: `rs/moq-cli/src/webrtc.rs` `connect_import` creates the route
already announced, "so viewers can discover the broadcast while it
connects", and only `moq_webrtc::Client::pull` creates the catalog track
(`IngestSink::new` in `rs/moq-webrtc/src/client/whep.rs`), so the CLI has
nothing to wait on.

Recommended: `Client::pull` takes the catalog producer, as
`moq_rtsp::Client::pull` does, and the CLI creates the route hidden and
announces it after the catalog's first update, the way
`rs/moq-cli/src/rtsp.rs` does. moq-webrtc is unpublished, so the signature
change is free before the cut, which is why this is m2. A viewer that arrives before the WHEP session
negotiates then waits on the announcement instead of failing.
