//! Adapt backend poll sessions to moq-net without changing thread affinity.
mod adapter;
pub use adapter::{Error, RecvStream, SendStream, Session};

// The backend traits have no word for a reliable reset, so only moq-quic's
// sessions and streams can offer one, recognized by type.
fn reliable_reset(session: &dyn std::any::Any) -> bool {
	#[cfg(feature = "quic")]
	if let Some(session) = session.downcast_ref::<crate::quic::web::Session>() {
		return session.reliable_reset();
	}
	let _ = session;
	false
}

fn checkpoint(stream: &mut dyn std::any::Any) {
	#[cfg(feature = "quic")]
	if let Some(stream) = stream.downcast_mut::<crate::quic::web::SendStream>() {
		stream.checkpoint();
	}
	let _ = stream;
}
