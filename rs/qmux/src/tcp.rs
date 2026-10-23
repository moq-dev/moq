use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::build_stream_session;
use crate::{Config, Error, Session};

/// Connect a QMux client over plain TCP with one session configuration.
///
/// TCP has no ALPN; set `Config::protocol` to negotiate an application protocol.
pub async fn connect(addr: impl ToSocketAddrs, config: Config) -> Result<Session, Error> {
	let stream = TcpStream::connect(addr).await?;
	finish(stream, config, false).await
}

/// Accept a QMux session; drive each handshake independently of the listener loop.
pub async fn accept(stream: TcpStream, config: Config) -> Result<Session, Error> {
	finish(stream, config, true).await
}

async fn finish(stream: TcpStream, config: crate::Config, is_server: bool) -> Result<Session, Error> {
	// The kernel already tracks this socket's RTT (and, on Linux, its delivery
	// rate) from the TCP handshake, so the session can report both immediately
	// rather than waiting on its own QX_PING round trip. A socket we can't read
	// stats from just leaves them unreported.
	#[cfg(unix)]
	let socket_stats = crate::TcpStats::new(&stream)
		.ok()
		.map(|s| std::sync::Arc::new(s) as crate::SharedSocketStats);
	#[cfg(not(unix))]
	let socket_stats = None;

	// `build_stream_session` awaits the peer's transport parameters before
	// returning, so `protocol()` is resolved on the session we hand back
	// (bounded by the config's handshake timeout).
	build_stream_session(stream, config, is_server, socket_stats).await
}
