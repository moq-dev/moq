//! Hermetic element-boundary tests: behaviour reachable without a live MoQ session.
//!
//! Flows that need a connected session (multipad EOS aggregation, per-pad error propagation, remote
//! close) are validated against a real relay, separately from this hermetic suite.

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
	let sink = gst::ElementFactory::make("moqsink").build().expect("create moqsink");

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
	let sink = gst::ElementFactory::make("moqsink").build().expect("create moqsink");
	assert!(
		sink.set_state(gst::State::Paused).is_err(),
		"a missing url must fail the Ready->Paused state change"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A connect that cannot succeed does NOT post a fatal ERROR: the sink reconnects with backoff
// (issue #2212), so an unattended publisher survives a relay that is unreachable at startup or
// during an outage instead of tearing down the pipeline. It keeps retrying and stays disconnected.
// (A non-retryable failure, e.g. auth rejection, is still terminal; that path needs a live relay
// and is covered separately.) The `.invalid` host fails fast at DNS resolution, so the loop is
// already several retries deep within the window below.
#[test]
fn connect_failure_retries_without_erroring() {
	init();
	let pipeline = gst::Pipeline::new();
	let sink = gst::ElementFactory::make("moqsink")
		.property("url", "https://nonexistent.invalid:443")
		.property("broadcast", "test")
		.build()
		.expect("create moqsink");
	pipeline.add(&sink).expect("add sink to pipeline");

	assert!(
		pipeline.set_state(gst::State::Playing).is_ok(),
		"a valid url + broadcast must let the Ready->Playing change start (connect runs in the background)"
	);
	let bus = pipeline.bus().expect("pipeline bus");
	let msg = bus.timed_pop_filtered(gst::ClockTime::from_seconds(3), &[gst::MessageType::Error]);
	let connected = sink.property::<bool>("connected");
	let status = sink.property::<gstmoq::ConnectionStatus>("status");
	let send_bitrate = sink.property::<u64>("estimated-send-bitrate");
	let recv_bitrate = sink.property::<u64>("estimated-recv-bitrate");
	let _ = pipeline.set_state(gst::State::Null);

	assert!(
		msg.is_none(),
		"a failed connect must NOT post an ERROR: the sink retries (issue #2212)"
	);
	assert!(
		!connected,
		"a failed connect must leave connected = false while retrying"
	);
	// While retrying, status stays Disconnected (a transient retry, not the terminal Failed) and the
	// bitrate estimates read 0.
	assert_eq!(status, gstmoq::ConnectionStatus::Disconnected);
	assert_eq!(send_bitrate, 0);
	assert_eq!(recv_bitrate, 0);
}
