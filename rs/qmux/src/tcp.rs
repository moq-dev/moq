use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::{Stream, build_stream_session};
use crate::{Config, Driver, Error, Session};

/// Connect a QMux client over plain TCP with one session configuration.
///
/// TCP has no ALPN; set `Config::protocol` to negotiate an application protocol.
pub async fn connect(addr: impl ToSocketAddrs, config: Config) -> Result<(Session, Driver<Stream<TcpStream>>), Error> {
	let stream = TcpStream::connect(addr).await?;
	finish(stream, config, false).await
}

/// Accept a QMux session; drive each handshake independently of the listener loop.
pub async fn accept(stream: TcpStream, config: Config) -> Result<(Session, Driver<Stream<TcpStream>>), Error> {
	finish(stream, config, true).await
}

async fn finish(
	stream: TcpStream,
	config: crate::Config,
	is_server: bool,
) -> Result<(Session, Driver<Stream<TcpStream>>), Error> {
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
/// the peer's delayed ACK. A session started with the returned handle holds the
/// kernel's unsent queue to about one congestion window (`TCP_NOTSENT_LOWAT` on
/// Linux, a send buffer of two windows on macOS), so what it has not sent yet
/// stays where a reset stream can still drop it.
///
/// For a socket whose session starts elsewhere, such as
/// `tls::Server::accept_established`.
pub fn adopt(stream: &TcpStream) -> std::io::Result<Option<crate::SharedSocketStats>> {
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

	/// Both ends of a session disable Nagle and hold the kernel's unsent queue to the
	/// congestion window. The client half is `finish`, since `connect` is only
	/// `TcpStream::connect` in front of it.
	#[tokio::test]
	async fn sessions_prepare_their_sockets() {
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
		let _client = client.unwrap();
		let _server = server.unwrap();
		for probe in probes {
			assert!(probe.nodelay().unwrap());
			#[cfg(any(target_os = "linux", target_os = "macos"))]
			{
				// The kernel's default bounds nothing (Linux) or autotunes well below it (macOS).
				// The window the bound came from moves slightly once the handshake settles the
				// MSS, and macOS rounds a send buffer up.
				let (set, expected) = crate::socket::unsent_bound(&probe);
				assert!((expected * 3 / 4..=expected * 2).contains(&set), "{set} vs {expected}");
			}
		}
	}

	/// As a wire source, a socket's statistics fill the counters with what the kernel says
	/// each side carried, the TCP payload without headers.
	#[cfg(target_os = "linux")]
	#[test]
	fn socket_statistics_refresh_wire_counters() {
		use moq_net::transport::{Wire, WireSource};
		use std::io::{Read, Write};

		let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
		let (mut server, _) = listener.accept().unwrap();
		client.write_all(&[7; 1000]).unwrap();
		server.read_exact(&mut [0; 1000]).unwrap();

		let (sent, received) = (Wire::new(), Wire::new());
		crate::TcpStats::new(&client).unwrap().refresh(&sent);
		crate::TcpStats::new(&server).unwrap().refresh(&received);
		assert_eq!((sent.sent_bytes(), sent.received_bytes()), (1000, 0));
		assert_eq!((received.sent_bytes(), received.received_bytes()), (0, 1000));
	}

	/// A driver that ends without closing its transport, such as one dropped, still ends
	/// the connection while a session handle (and the socket statistics' duplicate of the
	/// descriptor) outlives it.
	#[tokio::test]
	async fn dropped_driver_ends_the_connection() {
		use moq_net::transport::poll::Session as _;

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let mut config = Config::new(Version::QMux01);
		// Only the closed socket may end the server's session, not its idle timer.
		config.idle_timeout = None;
		let (client, server) = tokio::join!(connect(addr, config.clone()), async {
			accept(listener.accept().await.unwrap().0, config.clone()).await
		});
		let (client, client_driver) = client.unwrap();
		let (server, server_driver) = server.unwrap();
		tokio::spawn(server_driver);

		drop(client_driver);
		// Bounds a regression's hang; the close arrives in a round trip on loopback.
		kio::clock::timeout(std::time::Duration::from_secs(10), server.closed())
			.await
			.expect("the server never saw the connection end");
		drop(client);
	}
}
