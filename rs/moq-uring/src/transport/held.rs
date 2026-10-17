//! How the io_uring QUIC backend reads with connection credit.

use std::task::{Context, Poll};

use bytes::Bytes;

use crate::quic::{self as driver, web};

impl moq_net::transport::Credit for driver::Credit {
	fn len(&self) -> u64 {
		self.len()
	}

	fn split(&mut self, bytes: u64) -> Self {
		self.split(bytes)
	}

	fn merge(&mut self, other: Self) {
		self.merge(other)
	}
}

impl moq_net::transport::Stopper for web::Stopper {
	fn stop(&self, code: u32) {
		web::Stopper::stop(self, code)
	}
}

impl super::HeldRecvStream for web::RecvStream {
	type Credit = driver::Credit;
	type Stopper = web::Stopper;

	fn stopper(&self) -> web::Stopper {
		web::RecvStream::stopper(self)
	}

	fn poll_read_held(
		&mut self,
		cx: &mut Context<'_>,
		max: usize,
	) -> Poll<Result<Option<(Bytes, driver::Credit)>, Self::Error>> {
		web::RecvStream::poll_read_held(self, cx, max)
	}
}

impl super::HeldSession for web::Session {
	fn poll_credit_exhausted(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		web::Session::poll_credit_exhausted(self, cx)
	}
}
