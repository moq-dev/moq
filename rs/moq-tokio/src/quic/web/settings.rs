use futures::try_join;

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
	ConnectionError(#[from] crate::quic::driver::ConnectionError),

	#[error("read error")]
	ReadError(#[from] crate::quic::driver::ReadError),

	#[error("write error")]
	WriteError(#[from] crate::quic::driver::WriteError),
}

pub struct Settings {
	// A reference to the send/recv stream, so we don't close it until dropped.
	#[allow(dead_code)]
	send: crate::quic::driver::SendStream,

	#[allow(dead_code)]
	recv: crate::quic::driver::RecvStream,
}

impl Settings {
	// Establish the H3 connection.
	pub async fn connect(conn: &crate::quic::driver::Connection) -> Result<Self, SettingsError> {
		let recv = Self::accept(conn);
		let send = Self::open(conn);

		// Run both tasks concurrently until one errors or they both complete.
		let (send, recv) = try_join!(send, recv)?;
		Ok(Self { send, recv })
	}

	async fn accept(conn: &crate::quic::driver::Connection) -> Result<crate::quic::driver::RecvStream, SettingsError> {
		let mut recv = conn.accept_uni().await?;
		let settings = web_transport_proto::Settings::read(&mut recv).await?;

		tracing::debug!(?settings, "received SETTINGS frame");

		if settings.supports_webtransport() == 0 {
			return Err(SettingsError::WebTransportUnsupported);
		}

		Ok(recv)
	}

	async fn open(conn: &crate::quic::driver::Connection) -> Result<crate::quic::driver::SendStream, SettingsError> {
		let mut settings = web_transport_proto::Settings::default();
		settings.enable_webtransport(1);

		tracing::debug!(?settings, "sending SETTINGS frame");

		let mut send = conn.open_uni(Default::default()).await?;
		settings.write(&mut send).await?;

		Ok(send)
	}
}
