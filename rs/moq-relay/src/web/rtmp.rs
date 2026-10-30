//! RTMP and RTMPS connections the public web listeners classified, for an embedder to serve.
//!
//! The relay speaks no RTMP itself: without a consumer taken from
//! [`Web::rtmp`](super::Web::rtmp) it logs and closes them.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

/// The RTMP connections accepted on the public web listeners.
pub struct Incoming(mpsc::Receiver<Stream>);

impl Incoming {
	/// The next RTMP connection, or `None` once the web listeners stop.
	pub async fn accept(&mut self) -> Option<Stream> {
		self.0.recv().await
	}
}

/// An RTMP connection, plaintext or with TLS already terminated, for `moq_rtmp::accept_stream`.
pub struct Stream(crate::listener::Stream);

impl Stream {
	/// The accepted TCP socket, for socket options and statistics.
	pub fn tcp(&self) -> &tokio::net::TcpStream {
		self.0.tcp()
	}
}

impl AsyncRead for Stream {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.0).poll_read(cx, buf)
	}
}

impl AsyncWrite for Stream {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.0).poll_write(cx, buf)
	}
	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.0).poll_flush(cx)
	}
	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.0).poll_shutdown(cx)
	}
}

/// Where the listeners send classified RTMP connections; empty refuses them.
#[derive(Clone, Default)]
pub(crate) struct Handoff(Option<mpsc::Sender<Stream>>);

impl Handoff {
	/// A handoff and the consumer it feeds.
	///
	/// One slot: each connection waits on its own task for the embedder to take it,
	/// which the listener's idle deadline bounds like any other stalled handshake.
	pub(crate) fn new() -> (Self, Incoming) {
		let (tx, rx) = mpsc::channel(1);
		(Self(Some(tx)), Incoming(rx))
	}

	/// Give `stream` to the consumer, or close it when there is none.
	pub(crate) async fn send(&self, stream: crate::listener::Stream) {
		// The embedder owns an RTMP session's lifetime, so the drain stops counting it here.
		drop(stream.admission().as_ref().and_then(crate::listener::Admission::take));
		let peer = stream.tcp().peer_addr().ok();
		let refused = match &self.0 {
			Some(tx) => tx.send(Stream(stream)).await.is_err(),
			None => true,
		};
		if refused {
			tracing::debug!(?peer, "closed an RTMP connection: nothing serves RTMP here");
		}
	}
}
