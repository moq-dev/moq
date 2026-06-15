use gst::glib;
use gst::prelude::*;

mod imp;
mod session;
mod timeline;

glib::wrapper! {
	pub struct MoqSinkSpike(ObjectSubclass<imp::MoqSinkSpike>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(
		Some(plugin),
		"moqsinkspike",
		gst::Rank::NONE,
		MoqSinkSpike::static_type(),
	)
}
