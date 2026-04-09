use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
	pub struct MoqSink(ObjectSubclass<imp::MoqSink>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(Some(plugin), "moqsink", gst::Rank::NONE, MoqSink::static_type())
}
