# moq-rtsp

RTSP camera ingest for Media over QUIC.

Pulls an IP camera's H.264 or H.265 video and AAC audio over RTSP and publishes
it into a MoQ broadcast without re-encoding. RTP rides the RTSP connection (TCP
interleaved), so run it on the camera's network and the camera needs no inbound
port. Audio the stack cannot play, such as G.711, is skipped with a warning.

`moq import rtsp` in [`moq-cli`](../moq-cli) wraps this crate with reconnect and
a fresh epoch per session.

## Library

`Client::pull` ingests exactly one session into a broadcast and catalog you
supply, and never reconnects. It returns `End::Closed` when the server ends the
session and `End::Jumped` when the session's RTP time jumps, finishing its
tracks either way, or an error after aborting them. Dropping the future stops
the session and finishes its tracks.

A broadcast name always means the same content, so a restarted timeline must
not continue the old broadcast: finish it and publish a fresh one under a new
epoch.

```rust
let client = moq_rtsp::Client::new("rtsp://admin:secret@192.168.1.64/stream1".parse()?)?;

loop {
	let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
	let mut broadcast = origin.publish("camera", route)?;
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default())?;

	let end = client.pull(&broadcast, &catalog).await;
	catalog.finish()?;
	broadcast.close();
	end?;
}
```

Two sessions, such as a camera's SD and HD URLs, can publish renditions of one
broadcast by sharing its catalog: run both pulls and restart both when either
returns. Each session's timestamps shift onto the catalog's clock.

Credentials ride the URL's userinfo, as every RTSP tool takes them; they are
taken out of the URL before dialing and never logged.

Pure Rust: RTSP and RTP depacketizing come from
[`retina`](https://github.com/scottlamb/retina).
