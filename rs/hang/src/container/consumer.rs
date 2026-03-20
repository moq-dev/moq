use super::{Frame, Timestamp};
use buf_list::BufList;

/// A frame returned by [`OrderedConsumer::read()`] with group context.
#[deprecated(note = "use moq_mux::consumer::OrderedFrame instead")]
#[derive(Clone, Debug)]
pub struct OrderedFrame {
	pub timestamp: Timestamp,
	pub payload: BufList,
	pub group: u64,
	pub index: usize,
}

#[allow(deprecated)]
impl OrderedFrame {
	pub fn is_keyframe(&self) -> bool {
		self.index == 0
	}
}

#[allow(deprecated)]
impl From<OrderedFrame> for Frame {
	fn from(ordered: OrderedFrame) -> Self {
		Frame {
			timestamp: ordered.timestamp,
			payload: ordered.payload,
		}
	}
}

/// Deprecated: use `moq_mux::consumer::OrderedConsumer` instead.
///
/// This stub exists only to provide a deprecation warning.
/// The implementation has been moved to `moq_mux::consumer::OrderedConsumer`.
#[deprecated(note = "use moq_mux::consumer::OrderedConsumer instead")]
pub struct OrderedConsumer {
	_private: (),
}

#[allow(deprecated)]
impl OrderedConsumer {
	pub fn new(_track: moq_lite::TrackConsumer, _max_latency: std::time::Duration) -> Self {
		panic!("hang::container::OrderedConsumer has been moved to moq_mux::consumer::OrderedConsumer")
	}

	pub async fn read(&mut self) -> Result<Option<OrderedFrame>, crate::Error> {
		panic!("hang::container::OrderedConsumer has been moved to moq_mux::consumer::OrderedConsumer")
	}
}
