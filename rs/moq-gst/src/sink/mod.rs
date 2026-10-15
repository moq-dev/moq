use gst::glib;
use gst::prelude::*;

mod imp;
mod pad;
mod request_pad;
mod session;
mod timeline;
mod wall;

#[cfg(test)]
pub(crate) use pad::h264_keyframe_au;

/// The `moqsink` publish connection lifecycle, exposed as its read-only `status` property.
pub use session::ConnectionStatus;

/// The wire container used for media tracks published by `moqsink`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "GstMoqSinkMediaContainer")]
#[non_exhaustive]
pub enum MediaContainer {
	/// Hang's original timestamp-prefixed media container.
	#[default]
	#[enum_value(name = "Legacy", nick = "legacy")]
	Legacy,
	/// Low Overhead Container from draft-ietf-moq-loc.
	#[enum_value(name = "LOC", nick = "loc")]
	Loc,
}

impl From<MediaContainer> for hang::catalog::Container {
	fn from(container: MediaContainer) -> Self {
		match container {
			MediaContainer::Legacy => Self::Legacy,
			MediaContainer::Loc => Self::Loc,
		}
	}
}

glib::wrapper! {
	/// The `moqsink` element: publishes its `sink_%u` pads as a single MoQ broadcast, writing each pad's
	/// frames directly into the moq producers from its streaming thread (no intermediate queue).
	pub struct MoqSink(ObjectSubclass<imp::MoqSink>)
		@extends gst::Element, gst::Object,
		@implements gst::ChildProxy;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(Some(plugin), "moqsink", gst::Rank::NONE, MoqSink::static_type())
}
