//! Hermetic element-boundary tests: behaviour reachable without a live MoQ session.
//!
//! Session-dependent flows (multipad EOS aggregation, per-pad errors, FLUSH, remote death) require a
//! real relay, so they live in the relay-backed harness, not here.

use std::sync::Once;

use gst::prelude::*;

fn init() {
	static INIT: Once = Once::new();
	INIT.call_once(|| {
		gst::init().unwrap();
		gstmoq::plugin_register_static().expect("register moq plugin");
	});
}

// Request pads appear and disappear through the real GObject boundary, with no session attached.
#[test]
fn request_and_release_sink_pads() {
	init();
	let sink = gst::ElementFactory::make("moqsinkspike")
		.build()
		.expect("create moqsinkspike");

	let pad0 = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(pad0.name().as_str(), "sink_0");
	let pad1 = sink.request_pad_simple("sink_1").expect("request sink_1");
	assert_eq!(sink.num_sink_pads(), 2);

	sink.release_request_pad(&pad1);
	assert_eq!(sink.num_sink_pads(), 1);
	sink.release_request_pad(&pad0);
	assert_eq!(sink.num_sink_pads(), 0);
}

// Settings are validated synchronously: a missing url fails the state change, not the bus.
#[test]
fn missing_url_fails_state_change() {
	init();
	let sink = gst::ElementFactory::make("moqsinkspike")
		.build()
		.expect("create moqsinkspike");
	assert!(
		sink.set_state(gst::State::Paused).is_err(),
		"a missing url must fail the Ready->Paused state change"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A connect that cannot succeed surfaces as an ERROR on the bus, not a silent log.
#[test]
fn connect_failure_posts_error_to_bus() {
	init();
	let pipeline = gst::Pipeline::new();
	let sink = gst::ElementFactory::make("moqsinkspike")
		.property("url", "https://nonexistent.invalid:443")
		.property("broadcast", "test")
		.build()
		.expect("create moqsinkspike");
	pipeline.add(&sink).expect("add sink to pipeline");

	let _ = pipeline.set_state(gst::State::Playing);
	let bus = pipeline.bus().expect("pipeline bus");
	let msg = bus.timed_pop_filtered(gst::ClockTime::from_seconds(20), &[gst::MessageType::Error]);
	let _ = pipeline.set_state(gst::State::Null);

	assert!(msg.is_some(), "a failed connect must post an ERROR to the bus");
}
