//! End-to-end SRT loopback: publish MPEG-TS in over SRT, read it back out over SRT.
//!
//! Drives the real `srt-tokio` stack (UDP handshake, TSBPD) against
//! [`moq_srt::run`], so it exercises the full ingest -> origin -> egress path the
//! in-crate unit tests skip. Uses real wall-clock time (no `tokio::time::pause`),
//! since the SRT sockets do too.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use srt_tokio::SrtSocket;
use tokio::net::UdpSocket;

/// A small real H.264 + AAC transport stream (bbb.mp4 remuxed with `ffmpeg -c copy`).
const BBB_TS: &[u8] = include_bytes!("test_data/bbb.ts");

/// 7 TS packets: the egress payload size moq-srt slices on, and a sane publish chunk.
const SRT_PAYLOAD: usize = 7 * 188;

/// Grab a free UDP port by binding ephemeral and reading it back. Mildly racy, but
/// the only way to avoid a hardcoded port colliding with another test in CI.
async fn free_port() -> u16 {
	UdpSocket::bind("127.0.0.1:0")
		.await
		.unwrap()
		.local_addr()
		.unwrap()
		.port()
}

/// Connect an SRT caller with the given stream id.
async fn call(addr: SocketAddr, stream_id: &str) -> SrtSocket {
	SrtSocket::builder()
		.call(addr, Some(stream_id))
		.await
		.expect("SRT caller connect")
}

/// Publish a TS stream in over `m=publish`, then read it back out over `m=request`,
/// confirming the bytes that come out the egress side are real TS packets.
#[tokio::test]
async fn publish_then_request_roundtrip() {
	let addr: SocketAddr = ([127, 0, 0, 1], free_port().await).into();

	// The gateway: `m=publish` ingests into this origin, `m=request` serves back out.
	let origin = moq_net::Origin::random().produce();
	let mut config = moq_srt::Config::default();
	config.listen = Some(addr);
	let gateway = tokio::spawn(moq_srt::run(origin, config));

	// Publisher: push the TS in, then hold the socket open. A clean close would end
	// the broadcast immediately, and bbb.ts is too short to win that race against the
	// requester's connect, so we keep it announced by never sending EOF.
	let mut publisher = call(addr, "#!::r=cam0,m=publish").await;
	for chunk in BBB_TS.chunks(SRT_PAYLOAD) {
		publisher
			.send((Instant::now(), Bytes::copy_from_slice(chunk)))
			.await
			.expect("publish send");
	}

	// Requester: pull it back out as TS. Allow generous room for the SRT handshake
	// plus the default 200ms TSBPD buffer before the first payload is released.
	let mut requester = call(addr, "#!::r=cam0,m=request").await;
	let (_origin_instant, payload) = tokio::time::timeout(Duration::from_secs(15), requester.next())
		.await
		.expect("egress produced no payload before timeout")
		.expect("egress stream ended without a payload")
		.expect("egress socket error");

	// The muxer emits whole 188-byte TS packets, each led by the 0x47 sync byte.
	assert_eq!(payload[0], 0x47, "egress payload is not a TS packet");
	assert_eq!(payload.len() % 188, 0, "egress payload is not whole TS packets");

	drop(publisher);
	drop(requester);
	gateway.abort();
}
