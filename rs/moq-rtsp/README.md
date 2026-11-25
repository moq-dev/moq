# moq-rtsp

RTSP camera ingest for Media over QUIC.

Pulls an IP camera's H.264 or H.265 video and AAC audio over RTSP and publishes
it into a MoQ broadcast without re-encoding. RTP rides the RTSP connection (TCP
interleaved), so run it on the camera's network and the camera needs no inbound
port. Audio the stack cannot play, such as G.711, is skipped with a warning.

`moq import rtsp` in [`moq-cli`](../moq-cli) wraps this crate with reconnect and
a fresh instance per session.

## Library

`Client::pull` ingests exactly one session into a broadcast and catalog you
supply, and never reconnects. It returns `End::Closed` when the server ends the
session and `End::Jumped` when the session's RTP time jumps, finishing its
tracks either way, or an error after aborting them. Dropping the future stops
the session and finishes its tracks.

A broadcast name always means the same content, so a restarted timeline must
not continue the old broadcast: finish it and publish a fresh one under a new
instance.

```rust
let client = moq_rtsp::Client::new("rtsp://admin:secret@192.168.1.64/stream1".parse()?)?;

loop {
	// Each route mints a fresh instance.
	let route = origin.route("camera", moq_net::origin::RouteOptions::default())?;
	let mut catalog = moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), Default::default())?;

	let end = client.pull(route.broadcast(), &catalog).await;
	catalog.finish()?;
	route.broadcast().close();
	end?;
}
```

Each frame lands on the catalog's clock at its capture, so `sync.clock` gives
its wall time: where the camera's RTCP sender report for its stream says it was
taken, or, for a stream without a usable report, no later than any of its
frames arrived, which trails the capture by the camera's encoding and the
network. A session waits up to one report interval (5 s) after its first frame
for every stream's report, holding the catalog and each stream's frames since
its latest keyframe meanwhile. Each stream is placed on its own, since a
camera's `rtptime` need not align them.

A clock fixed before the session, by a shared catalog or
`Config::with_clock`, is shifted onto. Two sessions, such as a camera's SD and
HD URLs, can publish renditions of one broadcast by sharing its catalog: their
frames line up by wall time. Run both pulls and restart both when either
returns.

Credentials ride the URL's userinfo, as every RTSP tool takes them; they are
taken out of the URL before dialing and never logged.

Pure Rust: RTSP and RTP depacketizing come from
[`retina`](https://github.com/scottlamb/retina).
