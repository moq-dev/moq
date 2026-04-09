use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
	pub struct MoqSrc(ObjectSubclass<imp::MoqSrc>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	gst::Element::register(Some(plugin), "moqsrc", gst::Rank::NONE, MoqSrc::static_type())
}
