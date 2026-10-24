use std::path::Path;
use tokio::net::UnixStream;

use crate::transport::build_stream_session;
use crate::{Config, Error, Session};

/// Connect a QMux client over a Unix socket with one session configuration.
///
/// A Unix socket negotiates its application protocol in-band through `Config::protocol`.
pub async fn connect(path: impl AsRef<Path>, config: Config) -> Result<Session, Error> {
	let stream = UnixStream::connect(path).await?;
	finish(stream, config, false).await
}

/// Accept a QMux session; drive each handshake independently of the listener loop.
pub async fn accept(stream: UnixStream, config: Config) -> Result<Session, Error> {
	finish(stream, config, true).await
}

async fn finish(stream: UnixStream, config: crate::Config, is_server: bool) -> Result<Session, Error> {
	// `build_stream_session` awaits the peer's transport parameters before
	// returning, so `protocol()` is resolved on the session we hand back
	// (bounded by the config's handshake timeout).
	// A Unix socket has no TCP_INFO to read; loopback latency is negligible
	// anyway, and QX_PING still covers the round trip.
	build_stream_session(stream, config, is_server, None).await
}
