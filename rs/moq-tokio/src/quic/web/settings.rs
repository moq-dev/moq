use std::pin::Pin;

use futures::{StreamExt, stream::FuturesUnordered, try_join};
use tokio::io::AsyncReadExt;

use super::session::{PendingUni, log_header_error, read_uni_header};
use web_transport_proto::StreamUni;

pub struct Settings {
	#[allow(
		dead_code,
		reason = "a reference to the send/recv stream, so we don't close it until dropped"
	)]
	send: crate::quic::driver::send::Stream,

	#[allow(dead_code, reason = "held so the stream stays open until dropped")]
	recv: crate::quic::driver::recv::Stream,

	/// The peer's other unidirectional streams that arrived before its control stream, their
	/// headers read or still reading, for the session to take over. Behind a lock only so the
	/// session can share its settings; it is taken before then.
	pub(crate) early: std::sync::Mutex<Vec<Pin<Box<PendingUni>>>>,
}

impl Settings {
	// Establish the H3 connection.
	pub async fn connect(conn: &crate::quic::driver::connection::Connection) -> Result<Self, super::Error> {
		let recv = Self::accept(conn);
		let send = Self::open(conn);

		// Run both tasks concurrently until one errors or they both complete.
		let (send, (recv, early)) = try_join!(send, recv)?;
		Ok(Self {
			send,
			recv,
			early: std::sync::Mutex::new(early),
		})
	}

	/// Find the peer's control stream and read its SETTINGS.
	///
	/// QPACK streams, and an eager peer's WebTransport streams, may arrive ahead of the control
	/// stream, so every arrival has its header read at once: a stream that sends part of its
	/// header and stalls cannot hold off a control stream that already arrived. The others are
	/// kept, not dropped, since closing a QPACK stream ends the peer's HTTP/3 connection. Each
	/// one kept holds the peer's stream credit, which bounds how many it can open first.
	async fn accept(
		conn: &crate::quic::driver::connection::Connection,
	) -> Result<(crate::quic::driver::recv::Stream, Vec<Pin<Box<PendingUni>>>), super::Error> {
		let mut pending = FuturesUnordered::<Pin<Box<PendingUni>>>::new();
		let mut early = Vec::<Pin<Box<PendingUni>>>::new();

		let mut recv = loop {
			tokio::select! {
				recv = kio::wait(|waiter| conn.poll_accept_uni(waiter)) => pending.push(Box::pin(read_uni_header(recv?))),
				Some(header) = pending.next() => match header {
					Ok((StreamUni::CONTROL, _, recv)) => break recv,
					Ok(header) => early.push(Box::pin(std::future::ready(Ok(header)))),
					Err(err) => log_header_error(err, "unidirectional"),
				},
			}
		};
		early.extend(pending);

		// Reading the header took the stream type, which the SETTINGS reader expects to see.
		let mut control = (&[0u8][..]).chain(&mut recv);
		let settings = web_transport_proto::Settings::read(&mut control).await?;

		tracing::debug!(?settings, "received SETTINGS frame");

		if settings.supports_webtransport() == 0 {
			return Err(super::HandshakeError::Unsupported.into());
		}

		Ok((recv, early))
	}
	async fn open(
		conn: &crate::quic::driver::connection::Connection,
	) -> Result<crate::quic::driver::send::Stream, super::Error> {
		let mut settings = web_transport_proto::Settings::default();
		settings.enable_webtransport(1);

		tracing::debug!(?settings, "sending SETTINGS frame");

		let mut send = conn.open_uni(Default::default()).await?;
		settings.write(&mut send).await?;

		Ok(send)
	}
}
