//! VP9 bridge.
//!
//! Keyframes are detected from the frame_type bit (RFC 8741 §3 / VP9 spec §6.2:
//! the second bit of the uncompressed header).

use crate::{Result, codec};

/// Forwards str0m's VP9 frames to a `.vp9` track, detecting keyframes inline.
pub struct Bridge {
	catalog: moq_mux::catalog::Producer,
	track: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
	announced: bool,
}

impl Bridge {
	/// Publish a `.vp9` track on `broadcast`; the catalog rendition is added on the first frame.
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = broadcast.create_track(
			broadcast.unique_name(".vp9"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)?;
		let producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		Ok(Self {
			catalog,
			track: producer,
			announced: false,
		})
	}

	fn announce(&mut self) {
		if self.announced {
			return;
		}
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VP9::default());
		config.container = hang::catalog::Container::Legacy;
		self.catalog
			.lock()
			.video
			.renditions
			.insert(self.track.track().name().to_string(), config);
		self.announced = true;
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		self.announce();
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		let keyframe = is_keyframe(&frame.payload);
		self.track
			.write(moq_mux::container::Frame {
				timestamp: pts,
				payload: frame.payload,
				keyframe,
				duration: None,
			})
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("vp9 track write failed: {err}")))?;
		Ok(())
	}
}

/// Detect a VP9 keyframe from the uncompressed header's first byte (VP9 spec
/// §6.2), reading bits MSB-first: `frame_marker(2)`, `profile_low(1)`,
/// `profile_high(1)`, a `reserved(1)` bit only when profile == 3,
/// `show_existing_frame(1)`, then `frame_type(1)` (0 == KEY_FRAME). A
/// show-existing frame carries no frame_type and is never a keyframe.
fn is_keyframe(payload: &[u8]) -> bool {
	let Some(&b) = payload.first() else {
		return false;
	};
	let profile = (((b >> 4) & 1) << 1) | ((b >> 5) & 1); // (high << 1) | low
	// Bits consumed from the MSB: 2 (marker) + 2 (profile), plus profile 3's reserved bit.
	let mut pos = 4;
	if profile == 3 {
		pos += 1;
	}
	let show_existing_frame = (b >> (7 - pos)) & 1;
	if show_existing_frame == 1 {
		return false;
	}
	pos += 1;
	let frame_type = (b >> (7 - pos)) & 1;
	frame_type == 0
}

impl Drop for Bridge {
	fn drop(&mut self) {
		self.catalog.lock().video.renditions.remove(self.track.track().name());
	}
}

#[cfg(test)]
mod tests {
	use super::is_keyframe;

	// frame_marker = 0b10 in the top two bits for every well-formed header.
	#[test]
	fn profile0_keyframe_and_interframe() {
		// profile 0, show_existing_frame = 0, frame_type = 0 (key) / 1 (inter).
		assert!(is_keyframe(&[0b1000_0010]));
		assert!(!is_keyframe(&[0b1000_0110]));
	}

	#[test]
	fn profile0_show_existing_frame_is_not_keyframe() {
		// profile 0, show_existing_frame = 1: no frame_type follows.
		assert!(!is_keyframe(&[0b1000_1000]));
	}

	#[test]
	fn profile3_keyframe_and_interframe() {
		// profile 3 (both profile bits set) inserts a reserved bit before
		// show_existing_frame, shifting frame_type one position right.
		assert!(is_keyframe(&[0b1011_0000])); // reserved=0, show=0, frame_type=0
		assert!(!is_keyframe(&[0b1011_0010])); // frame_type=1
	}

	#[test]
	fn empty_payload_is_not_keyframe() {
		assert!(!is_keyframe(&[]));
	}
}
