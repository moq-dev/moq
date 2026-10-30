//! Splits a public web connection into HTTP and RTMP by its first bytes.
//!
//! `axum_server` runs the acceptor on each connection's own task, so a peer that
//! connects and sends nothing stalls only itself, under the same idle deadline
//! that bounds its TLS handshake and first request.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll},
};

use axum_server::{accept::Accept, tls_rustls::RustlsAcceptor};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use super::Accepted;

/// The RTMP version byte (C0) an RTMP handshake opens with.
const RTMP_VERSION: u8 = 0x03;

/// A classified connection: the protocol its first bytes named.
pub(crate) enum Classified {
	Http(Stream),
	Rtmp(Stream),
}

/// Classify `accepted`, terminating TLS with `tls` when the listener serves it.
///
/// A plaintext C0 is RTMP on either listener. On a TLS listener anything else is
/// handed to rustls, which refuses whatever is not a ClientHello (0x16), so
/// plaintext HTTP is still refused there. A negotiated ALPN names HTTP; RTMPS
/// clients offer none, so a connection without one is sniffed again on its first
/// decrypted byte.
pub(crate) async fn classify(accepted: Accepted, tls: Option<&RustlsAcceptor>) -> io::Result<Classified> {
	let mut first = [0u8; 1];
	if accepted.stream.peek(&mut first).await? == 0 {
		return Err(io::ErrorKind::UnexpectedEof.into());
	}
	let Some(tls) = tls.filter(|_| first[0] != RTMP_VERSION) else {
		let stream = Stream {
			transport: Transport::Plain(accepted),
			peeked: None,
		};
		return Ok(match first[0] {
			RTMP_VERSION => Classified::Rtmp(stream),
			_ => Classified::Http(stream),
		});
	};

	let (mut tls, ()) = tls.accept(accepted, ()).await?;
	if tls.get_ref().1.alpn_protocol().is_some() {
		return Ok(Classified::Http(Stream {
			transport: Transport::Tls(Box::new(tls)),
			peeked: None,
		}));
	}
	let first = tls.read_u8().await?;
	let stream = Stream {
		transport: Transport::Tls(Box::new(tls)),
		peeked: Some(first),
	};
	Ok(match first {
		RTMP_VERSION => Classified::Rtmp(stream),
		_ => Classified::Http(stream),
	})
}

/// An accepted connection after classification, plaintext or with TLS terminated.
pub(crate) struct Stream {
	transport: Transport,
	/// The decrypted byte read to classify the connection, replayed to the first read.
	peeked: Option<u8>,
}

enum Transport {
	Plain(Accepted),
	Tls(Box<TlsStream<Accepted>>),
}

impl Stream {
	fn accepted(&self) -> &Accepted {
		match &self.transport {
			Transport::Plain(accepted) => accepted,
			Transport::Tls(tls) => tls.get_ref().0,
		}
	}

	/// The accepted socket, for socket options and kernel statistics.
	pub(crate) fn tcp(&self) -> &TcpStream {
		&self.accepted().stream
	}

	/// The TLS session, when the connection carried one.
	pub(crate) fn tls(&self) -> Option<&rustls::ServerConnection> {
		match &self.transport {
			Transport::Plain(_) => None,
			Transport::Tls(tls) => Some(tls.get_ref().1),
		}
	}

	pub(crate) fn admission(&self) -> Option<super::Admission> {
		self.accepted().admission.clone()
	}
}

impl AsyncRead for Stream {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		if buf.remaining() > 0
			&& let Some(byte) = self.peeked.take()
		{
			buf.put_slice(&[byte]);
			return Poll::Ready(Ok(()));
		}
		match &mut self.transport {
			Transport::Plain(accepted) => Pin::new(accepted).poll_read(cx, buf),
			Transport::Tls(tls) => Pin::new(tls.as_mut()).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for Stream {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		match &mut self.transport {
			Transport::Plain(accepted) => Pin::new(accepted).poll_write(cx, buf),
			Transport::Tls(tls) => Pin::new(tls.as_mut()).poll_write(cx, buf),
		}
	}
	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match &mut self.transport {
			Transport::Plain(accepted) => Pin::new(accepted).poll_flush(cx),
			Transport::Tls(tls) => Pin::new(tls.as_mut()).poll_flush(cx),
		}
	}
	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match &mut self.transport {
			Transport::Plain(accepted) => Pin::new(accepted).poll_shutdown(cx),
			Transport::Tls(tls) => Pin::new(tls.as_mut()).poll_shutdown(cx),
		}
	}
}

#[cfg(unix)]
impl std::os::fd::AsFd for Stream {
	fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
		std::os::fd::AsFd::as_fd(self.tcp())
	}
}
