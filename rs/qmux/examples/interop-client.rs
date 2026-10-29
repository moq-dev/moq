//! WebSocket client used by the cross-language QMux smoke test.
//!
//! The mirror of `interop-server`: here the TypeScript side accepts the socket
//! and this example dials it, so the roles that decide the stream-id space are
//! swapped. Completing the scenario proves a `Session.accept` server agrees with
//! an independent implementation about role assignment and the wire format.

mod common;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use common::*;
use moq_net::transport::poll::{SendOptions, Session as _};
use qmux::{Session, Version};

async fn run(session: Session, version: Version) -> anyhow::Result<()> {
	if session.protocol() != Some(PROTOCOL) {
		bail!("unexpected negotiated protocol: {:?}", session.protocol());
	}

	// Rust -> TypeScript.
	let mut send = kio::clock::timeout(TIMEOUT, session.open_uni(SendOptions::default()))
		.await
		.context("timed out opening the client unidirectional stream")??;
	send_all(&mut send, &payload(CLIENT_SEED), "the client flow-control payload").await?;

	// TypeScript -> Rust. Server-initiated streams are the ids only a
	// `Session::accept` session may mint; a client-role server would be rejected
	// here rather than accepted.
	let mut recv = kio::clock::timeout(TIMEOUT, session.accept_uni())
		.await
		.context("timed out accepting the server unidirectional stream")??;
	let received = recv_all(&mut recv, "the server flow-control payload").await?;
	if received.as_ref() != payload(SERVER_SEED) {
		bail!("server flow-control payload did not match");
	}

	// Exercise both halves of one bidirectional stream and FIN propagation.
	let (mut send, mut recv) = kio::clock::timeout(TIMEOUT, session.open_bi(SendOptions::default()))
		.await
		.context("timed out opening the bidirectional stream")??;
	let request = format!("ping:{}", version.alpn());
	send_all(&mut send, request.as_bytes(), "the bidirectional request").await?;
	let response = recv_all(&mut recv, "the bidirectional response").await?;
	if response.as_ref() != format!("pong:{}", version.alpn()).as_bytes() {
		bail!("unexpected bidirectional response");
	}

	// DATAGRAM is available only on record-framed drafts (QMux01+).
	if version.uses_records() {
		session.send_datagram(Bytes::from_static(b"rust-datagram"), SendOptions::default())?;
		let datagram = kio::clock::timeout(TIMEOUT, session.recv_datagram())
			.await
			.context("timed out receiving the TypeScript datagram")??;
		if datagram.as_ref() != b"typescript-datagram" {
			bail!("unexpected TypeScript datagram");
		}
	} else if session.max_datagram_size() != 0 {
		bail!("QMux00 unexpectedly negotiated datagram support");
	}

	let closed = kio::clock::timeout(TIMEOUT, session.closed())
		.await
		.context("timed out waiting for the TypeScript server to close")?;
	match closed {
		qmux::Error::ConnectionClosed { code: 42, reason } if reason == "interop complete" => {}
		other => bail!("unexpected session close: {other}"),
	}

	Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let url = args
		.next()
		.context("usage: interop-client <url> <qmux-00|qmux-01|qmux-02>")?;
	let raw_version = args
		.next()
		.context("usage: interop-client <url> <qmux-00|qmux-01|qmux-02>")?;
	let version = parse_version(&raw_version)?;

	let session = qmux::ws::Client::new()
		.with_protocol(PROTOCOL, &[version])
		.require_protocol()
		.connect(&url)
		.await?;

	run(session, version).await
}
