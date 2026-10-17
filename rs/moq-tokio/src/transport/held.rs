//! How each backend reads with connection credit.

/// Read a plain chunk, whose connection credit the backend returned as it read.
#[cfg(any(test, feature = "tcp", feature = "websocket", feature = "iroh"))]
pub(crate) async fn read_unheld<S: web_transport_trait::RecvStream>(
	stream: &mut S,
	max: usize,
) -> Result<Option<(bytes::Bytes, moq_net::transport::Unheld)>, S::Error> {
	let chunk = stream.read_chunk(max).await?;
	Ok(chunk.map(|bytes| {
		let len = bytes.len() as u64;
		(bytes, moq_net::transport::Unheld(len))
	}))
}

/// Give a backend that returns connection credit as it reads, so never fills its window with
/// held credit, the held interface.
#[cfg(any(test, feature = "tcp", feature = "websocket", feature = "iroh"))]
macro_rules! unheld {
	($session:ty, $recv:ty) => {
		impl $crate::transport::HeldRecvStream for $recv {
			type Credit = moq_net::transport::Unheld;
			type Stopper = ();

			fn stopper(&self) {}

			fn read_chunk_held(
				&mut self,
				max: usize,
			) -> impl Future<Output = Result<Option<(bytes::Bytes, Self::Credit)>, Self::Error>> + Send {
				$crate::transport::held::read_unheld(self, max)
			}
		}

		impl $crate::transport::HeldSession for $session {
			fn poll_credit_exhausted(&self, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
				std::task::Poll::Pending
			}
		}
	};
}
#[cfg(test)]
pub(crate) use unheld;

#[cfg(any(feature = "tcp", feature = "websocket"))]
unheld!(qmux::Session, qmux::RecvStream);
// iroh's upstream quinn has no way to hold credit, so its incomplete messages are bounded
// only by its stream limit, each up to the largest message.
#[cfg(feature = "iroh")]
unheld!(web_transport_iroh::Session, web_transport_iroh::RecvStream);

#[cfg(feature = "quic")]
impl moq_net::transport::Credit for crate::quic::driver::Credit {
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

#[cfg(feature = "quic")]
impl moq_net::transport::Stopper for crate::quic::web::Stopper {
	fn stop(&self, code: u32) {
		// Err means the stream is already gone, which is what we wanted.
		let _ = crate::quic::web::Stopper::stop(self, code);
	}
}

#[cfg(feature = "quic")]
impl super::HeldRecvStream for crate::quic::web::RecvStream {
	type Credit = crate::quic::driver::Credit;
	type Stopper = crate::quic::web::Stopper;

	fn stopper(&self) -> Self::Stopper {
		crate::quic::web::RecvStream::stopper(self)
	}

	fn read_chunk_held(
		&mut self,
		max: usize,
	) -> impl Future<Output = Result<Option<(bytes::Bytes, Self::Credit)>, Self::Error>> + Send {
		crate::quic::web::RecvStream::read_chunk(self, max)
	}
}

#[cfg(feature = "quic")]
impl super::HeldSession for crate::quic::web::Session {
	fn poll_credit_exhausted(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
		crate::quic::driver::Connection::poll_credit_exhausted(self, cx)
	}
}
