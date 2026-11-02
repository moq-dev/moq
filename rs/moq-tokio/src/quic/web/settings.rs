use std::pin::Pin;

use futures::{StreamExt, stream::FuturesUnordered, try_join};
use tokio::io::AsyncReadExt;

use super::proto::StreamUni;
use super::session::{PendingUni, log_header_error, read_uni_header};

use thiserror::Error;

#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum SettingsError {
	#[error("quic stream was closed early")]
	UnexpectedEnd,

	#[error("protocol error: {0}")]
	ProtoError(#[from] web_transport_proto::SettingsError),

	#[error("WebTransport is not supported")]
	WebTransportUnsupported,

	#[error("connection error")]
	ConnectionError(#[from] crate::quic::driver::connection::Error),

	#[error("read error")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	#[error("write error")]
	WriteError(#[from] crate::quic::driver::send::Error),
}

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
	pub async fn connect(conn: &crate::quic::driver::connection::Connection) -> Result<Self, SettingsError> {
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
	) -> Result<(crate::quic::driver::recv::Stream, Vec<Pin<Box<PendingUni>>>), SettingsError> {
		let mut accept = std::pin::pin!(futures::stream::unfold(conn.clone(), |conn| async {
			Some((conn.accept_uni().await, conn))
		}));
		let mut pending = FuturesUnordered::<Pin<Box<PendingUni>>>::new();
		let mut early = Vec::<Pin<Box<PendingUni>>>::new();

		let mut recv = loop {
			tokio::select! {
				Some(recv) = accept.next() => pending.push(Box::pin(read_uni_header(recv?))),
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
			return Err(SettingsError::WebTransportUnsupported);
		}

		Ok((recv, early))
	}
	async fn open(
		conn: &crate::quic::driver::connection::Connection,
	) -> Result<crate::quic::driver::send::Stream, SettingsError> {
		let mut settings = web_transport_proto::Settings::default();
		settings.enable_webtransport(1);

		tracing::debug!(?settings, "sending SETTINGS frame");

		let mut send = conn.open_uni(Default::default()).await?;
		settings.write(&mut send).await?;

		Ok(send)
	}
}
