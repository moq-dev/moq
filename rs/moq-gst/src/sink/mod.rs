use gst::glib;
use gst::prelude::*;

mod imp;
mod pad;
mod session;
mod timeline;

glib::wrapper! {
	/// The `moqsink` element: publishes its `sink_%u` pads as a single MoQ broadcast. Built on
	/// `GstAggregator`, which owns the per-pad queues and FLUSH/EOS handling.
	pub struct MoqSink(ObjectSubclass<imp::MoqSink>) @extends gst_base::Aggregator, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(Some(plugin), "moqsink", gst::Rank::NONE, MoqSink::static_type())
}
