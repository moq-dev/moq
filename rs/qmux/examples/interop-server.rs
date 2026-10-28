//! WebSocket server used by the cross-language QMux smoke test.
//!
//! The TypeScript client starts this example once per supported QMux draft. The
//! server accepts only the requested draft, so completing the scenario proves
//! the WebSocket subprotocol negotiation and both implementations' wire formats
//! agree.

mod common;

use std::io::Write as _;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use common::*;
use moq_net::transport::poll::{SendOptions, Session as _};
use qmux::{Session, Version};
use tokio::net::TcpListener;

async fn run(session: Session, version: Version) -> anyhow::Result<()> {
	if session.protocol() != Some(PROTOCOL) {
		bail!("unexpected negotiated protocol: {:?}", session.protocol());
	}

	// TypeScript -> Rust, with the default Rust windows.
	let mut recv = moq_clock::timeout(TIMEOUT, session.accept_uni())
		.await
		.context("timed out accepting the client unidirectional stream")??;
	let received = recv_all(&mut recv, "the client flow-control payload").await?;
	if received.as_ref() != payload(CLIENT_SEED) {
		bail!("client flow-control payload did not match");
	}

	// Rust -> TypeScript. The client advertises deliberately small receive
	// windows, so this cannot finish unless it consumes and replenishes credit.
	let mut send = moq_clock::timeout(TIMEOUT, session.open_uni(SendOptions::default()))
		.await
		.context("timed out opening the server unidirectional stream")??;
	send_all(&mut send, &payload(SERVER_SEED), "the server flow-control payload").await?;

	// Exercise both halves of one bidirectional stream and FIN propagation.
	let (mut send, mut recv) = moq_clock::timeout(TIMEOUT, session.accept_bi())
		.await
		.context("timed out accepting the bidirectional stream")??;
	let request = recv_all(&mut recv, "the bidirectional request").await?;
	if request.as_ref() != format!("ping:{}", version.alpn()).as_bytes() {
		bail!("unexpected bidirectional request");
	}
	let response = format!("pong:{}", version.alpn());
	send_all(&mut send, response.as_bytes(), "the bidirectional response").await?;

	// DATAGRAM is available only on record-framed drafts (QMux01+).
	if version.uses_records() {
		session.send_datagram(Bytes::from_static(b"rust-datagram"), SendOptions::default())?;
		let datagram = moq_clock::timeout(TIMEOUT, session.recv_datagram())
			.await
			.context("timed out receiving the TypeScript datagram")??;
		if datagram.as_ref() != b"typescript-datagram" {
			bail!("unexpected TypeScript datagram");
		}
	} else if session.max_datagram_size() != 0 {
		bail!("QMux00 unexpectedly negotiated datagram support");
	}

	let closed = moq_clock::timeout(TIMEOUT, session.closed())
		.await
		.context("timed out waiting for the TypeScript client to close")?;
	match closed {
		qmux::Error::ConnectionClosed { code: 42, reason } if reason == "interop complete" => {}
		other => bail!("unexpected session close: {other}"),
	}

	Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let raw_version = std::env::args()
		.nth(1)
		.context("usage: interop-server <qmux-00|qmux-01|qmux-02>")?;
	let version = parse_version(&raw_version)?;

	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let addr = listener.local_addr()?;
	println!("ws://{addr}/interop");
	std::io::stdout().flush()?;

	let (socket, _) = listener.accept().await?;
	socket.set_nodelay(true)?;
	let session = qmux::ws::Server::new()
		.with_protocol(PROTOCOL, &[version])
		.require_protocol()
		.accept(socket)
		.await?;

	run(session, version).await
}
