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
	let socket_stats = adopt(&stream)?;
	// `build_stream_session` awaits the peer's transport parameters before
	// returning, so `protocol()` is resolved on the session we hand back
	// (bounded by the config's handshake timeout).
	build_stream_session(stream, config, is_server, socket_stats).await
}

/// Prepare a TCP socket to carry a session, returning the kernel's view of it when
/// this platform exposes one.
///
/// Disables Nagle: the session already batches records and flushes once per batch,
/// so Nagle has nothing left to coalesce and would only hold a batch's tail until
/// the peer's delayed ACK.
pub(crate) fn adopt(stream: &TcpStream) -> std::io::Result<Option<crate::SharedSocketStats>> {
	stream.set_nodelay(true)?;

	// The kernel already tracks this socket's RTT (and, on Linux, its delivery
	// rate) from the TCP handshake, so the session can report both immediately
	// rather than waiting on its own QX_PING round trip. A socket we can't read
	// stats from just leaves them unreported.
	#[cfg(any(target_os = "linux", target_os = "macos"))]
	{
		Ok(crate::TcpStats::new(stream)
			.ok()
			.map(|s| std::sync::Arc::new(s) as crate::SharedSocketStats))
	}
	#[cfg(not(any(target_os = "linux", target_os = "macos")))]
	{
		Ok(None)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Version;

	/// Both ends of a session disable Nagle. The client half is `finish`, since
	/// `connect` is only `TcpStream::connect` in front of it.
	#[tokio::test]
	async fn sessions_disable_nagle() {
		let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
		let (server, _) = listener.accept().unwrap();
		// A clone shares the socket, so it sees options set through the original.
		let probes = [client.try_clone().unwrap(), server.try_clone().unwrap()];
		let adopt = |stream: std::net::TcpStream| {
			stream.set_nonblocking(true).unwrap();
			TcpStream::from_std(stream).unwrap()
		};

		let config = Config::new(Version::QMux01);
		let (client, server) = tokio::join!(
			finish(adopt(client), config.clone(), false),
			accept(adopt(server), config)
		);
		client.unwrap();
		server.unwrap();
		for probe in probes {
			assert!(probe.nodelay().unwrap());
		}
	}
}
