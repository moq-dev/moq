use serde::{Deserialize, Serialize};

/// Detection metadata in the catalog.
///
/// This describes a track containing live object detection results
/// (bounding boxes), typically produced by an AI worker analyzing the
/// video stream. The catalog only carries the track reference; the
/// actual detections are sent as JSON frames on the referenced track.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
	/// The track containing the live detection updates.
	///
	/// Each frame on this track is a [`Detections`] JSON payload describing
	/// the bounding boxes detected in the most recent video frame(s).
	pub track: Option<moq_lite::Track>,
}

/// A single detection result for one object in a video frame.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DetectionBox {
	/// The X coordinate of the top-left corner, normalized from 0 to 1.
	pub x: f32,

	/// The Y coordinate of the top-left corner, normalized from 0 to 1.
	pub y: f32,

	/// The width of the bounding box, normalized from 0 to 1.
	pub w: f32,

	/// The height of the bounding box, normalized from 0 to 1.
	pub h: f32,

	/// A human readable label for the detected object (e.g. "person").
	pub label: Option<String>,

	/// Detection confidence, from 0 to 1.
	pub score: Option<f32>,
}

/// The payload of a single frame on a detection track.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Detections {
	/// The presentation timestamp (in microseconds) of the video frame
	/// these detections were computed from. Optional; if absent, consumers
	/// should treat the detections as applying to the current video frame.
	pub timestamp: Option<u64>,

	/// The bounding boxes detected in the frame.
	#[serde(default)]
	pub boxes: Vec<DetectionBox>,
}
