use bytes::{Buf, BufMut, Bytes};
use moq_net::transport::{self as moq, poll};
use std::task::{Context, Poll};
use web_transport_trait::{self as wt, poll as backend};

/// A backend failure preserving the session and stream code registries.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error<E>(E);

impl<E: wt::Error> moq::Error for Error<E> {
	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}
	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

struct Stats<S>(S);
impl<S: wt::Stats> moq::Stats for Stats<S> {
	fn bytes_sent(&self) -> Option<u64> {
		self.0.bytes_sent()
	}
	fn bytes_received(&self) -> Option<u64> {
		self.0.bytes_received()
	}
	fn bytes_lost(&self) -> Option<u64> {
		self.0.bytes_lost()
	}
	fn packets_sent(&self) -> Option<u64> {
		self.0.packets_sent()
	}
	fn packets_received(&self) -> Option<u64> {
		self.0.packets_received()
	}
	fn packets_lost(&self) -> Option<u64> {
		self.0.packets_lost()
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.0.rtt()
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.0.estimated_send_rate()
	}
}
impl<S: wt::Session<SendStream: 'static, RecvStream: 'static>> poll::Session for super::Session<S> {
	type SendStream = super::SendStream<S::SendStream>;
	type RecvStream = super::RecvStream<S::RecvStream>;
	type Error = Error<S::Error>;
	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		backend::Session::poll_accept_uni(self, cx).map_err(Error)
	}
	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_accept_bi(self, cx).map_err(Error)
	}
	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		backend::Session::poll_open_uni(self, cx).map_err(Error)
	}
	fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_open_bi(self, cx).map_err(Error)
	}
	fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		backend::Session::poll_send_datagram(self, cx, payload).map_err(Error)
	}
	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		backend::Session::poll_recv_datagram(self, cx).map_err(Error)
	}
	fn max_datagram_size(&self) -> usize {
		backend::Session::max_datagram_size(self)
	}
	fn protocol(&self) -> Option<&str> {
		backend::Session::protocol(self)
	}
	fn close(&mut self, code: u32, reason: &str) {
		backend::Session::close(self, code, reason)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		backend::Session::poll_closed(self, cx).map(Error)
	}
	fn stats(&self) -> impl moq::Stats {
		Stats(backend::Session::stats(self))
	}
}
impl<S: wt::SendStream + 'static> poll::SendStream for super::SendStream<S> {
	type Error = Error<S::Error>;
	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write(self, cx, buf).map_err(Error)
	}
	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write_buf(self, cx, buf).map_err(Error)
	}
	fn set_priority(&mut self, order: i32) {
		backend::SendStream::set_priority(self, order)
	}
	fn finish(&mut self) -> Result<(), Self::Error> {
		backend::SendStream::finish(self).map_err(Error)
	}
	fn reset(&mut self, code: u32) {
		backend::SendStream::reset(self, code)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		backend::SendStream::poll_closed(self, cx).map_err(Error)
	}
}
impl<S: wt::RecvStream + 'static> poll::RecvStream for super::RecvStream<S> {
	type Error = Error<S::Error>;
	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		backend::RecvStream::poll_read(self, cx, dst).map_err(Error)
	}
	fn poll_read_buf<B: BufMut>(
		&mut self,
		cx: &mut Context<'_>,
		buf: &mut B,
	) -> Poll<Result<Option<usize>, Self::Error>> {
		backend::RecvStream::poll_read_buf(self, cx, buf).map_err(Error)
	}
	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		backend::RecvStream::poll_read_chunk(self, cx, max).map_err(Error)
	}
	fn stop(&mut self, code: u32) {
		backend::RecvStream::stop(self, code)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		backend::RecvStream::poll_closed(self, cx).map_err(Error)
	}
}
