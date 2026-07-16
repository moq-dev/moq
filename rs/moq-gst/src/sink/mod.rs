use gst::glib;
use gst::prelude::*;

mod imp;
mod pad;
mod session;
mod timeline;

/// The `moqsink` publish connection lifecycle, exposed as its read-only `status` property.
pub use session::ConnectionStatus;

glib::wrapper! {
	/// The `moqsink` element: publishes its `sink_%u` pads as a single MoQ broadcast, writing each pad's
	/// frames directly into the moq producers from its streaming thread (no intermediate queue).
	pub struct MoqSink(ObjectSubclass<imp::MoqSink>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(Some(plugin), "moqsink", gst::Rank::NONE, MoqSink::static_type())
}
