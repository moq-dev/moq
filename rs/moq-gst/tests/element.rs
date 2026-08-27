//! Hermetic element-boundary tests: behaviour reachable without a live MoQ session.
//!
//! Flows that need a connected session (multipad EOS aggregation, per-pad error propagation, remote
//! close) are validated against a real relay, separately from this hermetic suite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};

use gst::prelude::*;

fn init() {
	static INIT: Once = Once::new();
	INIT.call_once(|| {
		gst::init().unwrap();
		gstmoq::plugin_register_static().expect("register moq plugin");
	});
}

fn child_of(sink: &gst::Element, name: &str) -> gst::glib::Object {
	sink.dynamic_cast_ref::<gst::ChildProxy>()
		.expect("moqsink implements GstChildProxy")
		.child_by_name(name)
		.expect("the request pad is a child")
}

/// A publisher pointed at a relay that cannot answer. Connect runs in the background, so the element
/// still reaches PAUSED and creates its producers, which is all these tests need.
fn publisher() -> gst::Element {
	gst::ElementFactory::make("moqsink")
		.property("url", "https://127.0.0.1:1")
		.property("broadcast", "test")
		.build()
		.expect("create moqsink")
}

/// Every message type the element posted, in order.
fn recording_bus(sink: &gst::Element) -> gst::Bus {
	let bus = gst::Bus::new();
	sink.set_bus(Some(&bus));
	bus
}

fn posted_eos(bus: &gst::Bus) -> usize {
	let mut seen = 0;
	while let Some(message) = bus.pop() {
		if message.type_() == gst::MessageType::Eos {
			seen += 1;
		}
	}
	seen
}

fn h264_caps() -> gst::Caps {
	gst::Caps::builder("video/x-h264")
		.field("stream-format", "byte-stream")
		.field("alignment", "au")
		.build()
}

/// Drive one pad to the point where it reserves its track: STREAM_START keeps the sticky events in
/// order, CAPS builds the producer.
fn send_caps(pad: &gst::Pad) -> bool {
	pad.send_event(gst::event::StreamStart::new("test")) && pad.send_event(gst::event::Caps::new(&h264_caps()))
}

// A flush re-anchors the pad's timeline and it flows again, so it must stop counting towards the
// element's EOS aggregation. Leaving it ended let the *next* pad's EOS complete the element while this
// one was still publishing.
#[test]
fn a_flush_after_eos_makes_the_pad_flow_again() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused).expect("start the publication");
	let bus = recording_bus(&sink);

	assert!(first.send_event(gst::event::Eos::new()));
	assert!(first.send_event(gst::event::FlushStart::new()));
	assert!(first.send_event(gst::event::FlushStop::builder(true).build()));
	assert!(second.send_event(gst::event::Eos::new()));

	assert_eq!(
		posted_eos(&bus),
		0,
		"the flushed pad is publishing again, so the element has not ended"
	);
	let _ = sink.set_state(gst::State::Null);
}

// STREAM_START is the same fresh start.
#[test]
fn a_new_stream_after_eos_makes_the_pad_flow_again() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused).expect("start the publication");
	let bus = recording_bus(&sink);

	assert!(first.send_event(gst::event::Eos::new()));
	assert!(first.send_event(gst::event::StreamStart::new("second-stream")));
	assert!(second.send_event(gst::event::Eos::new()));

	assert_eq!(posted_eos(&bus), 0, "the restarted pad has not ended");
	let _ = sink.set_state(gst::State::Null);
}

// Once the publication is finalized the producers are gone, and a flush cannot bring them back. The
// streaming thread gets an answer instead of an `Ok` written into nothing.
#[test]
fn a_buffer_after_the_publication_ended_reports_eos() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the publication");
	pad.set_active(true).expect("activate the pad");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));

	assert!(pad.send_event(gst::event::FlushStart::new()));
	assert!(pad.send_event(gst::event::FlushStop::builder(true).build()));
	assert_eq!(
		pad.chain(gst::Buffer::new()),
		Err(gst::FlowError::Eos),
		"the finalized publication answers rather than accepting the buffer"
	);
	let _ = sink.set_state(gst::State::Null);
}

// The flush window: a pad that has begun flushing must stop counting towards EOS right away. Waiting
// for FLUSH_STOP let pad B's EOS complete the element mid-flush, and the finalize that follows is not
// something pad A's FLUSH_STOP can undo.
#[test]
fn an_eos_during_another_pads_flush_does_not_complete_the_element() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused).expect("start the publication");
	let bus = recording_bus(&sink);

	assert!(first.send_event(gst::event::Eos::new()));
	assert!(first.send_event(gst::event::FlushStart::new()));
	assert!(second.send_event(gst::event::Eos::new()));

	assert_eq!(
		posted_eos(&bus),
		0,
		"the flushing pad is not ended, so the element has not completed"
	);
	let _ = sink.set_state(gst::State::Null);
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

// Request pads are children, so a pipeline description can name the track each one publishes
// (`moqsink sink_0::track=camera`). With no session nothing is reserved yet, so the property reads
// back what was asked for.
#[test]
fn sink_pads_are_named_through_child_proxy() {
	init();
	let sink = gst::ElementFactory::make("moqsink").build().expect("create moqsink");
	let proxy = sink
		.dynamic_cast_ref::<gst::ChildProxy>()
		.expect("moqsink implements GstChildProxy");

	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(proxy.children_count(), 1);
	let child = proxy.child_by_name("sink_0").expect("sink_0 is a child");
	child.set_property("track", "camera");
	assert_eq!(child.property::<String>("track"), "camera");

	// An empty name is not a track name: it selects the generated one.
	child.set_property("track", "");
	assert_eq!(child.property::<Option<String>>("track"), None);

	sink.release_request_pad(&pad);
	assert!(
		proxy.child_by_name("sink_0").is_none(),
		"a released pad is no longer a child"
	);
}

// The announced syntax, through the parser that users actually type. The pad does not exist when the
// description is parsed, so the value lands on it as a delayed child-proxy set once it is requested.
#[test]
fn a_pipeline_description_names_the_track() {
	init();
	let sink = gst::parse::launch("moqsink name=publisher url=https://127.0.0.1:1 broadcast=test sink_0::track=camera")
		.expect("parse the description");
	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(
		child_of(&sink, "sink_0").property::<String>("track"),
		"camera",
		"sink_0::track reached the pad"
	);
}

#[test]
fn a_pipeline_description_configures_quic_timeouts() {
	init();
	let sink = gst::parse::launch(
		"moqsink url=https://127.0.0.1:1 broadcast=test quic-idle-timeout=15000 quic-keep-alive=3000",
	)
	.expect("parse the description");
	assert_eq!(sink.property::<u64>("quic-idle-timeout"), 15_000);
	assert_eq!(sink.property::<u64>("quic-keep-alive"), 3_000);
}

#[test]
fn a_pipeline_description_selects_loc() {
	init();
	let sink =
		gst::parse::launch("moqsink name=publisher url=https://127.0.0.1:1 broadcast=test sink_0::container=loc")
			.expect("parse the description");
	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(
		child_of(&sink, "sink_0").property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Loc
	);
}

// The acceptance criterion: once CAPS reserves the track, its name and container are fixed; stopping
// the element makes both configurable again.
#[test]
fn a_reserved_name_reads_back_and_is_released_on_ready() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	let legacy_pad = sink.request_pad_simple("sink_1").expect("request sink_1");
	let legacy_child = child_of(&sink, "sink_1");
	child.set_property("track", "camera");
	child.set_property("container", gstmoq::MediaContainer::Loc);
	assert_eq!(
		legacy_child.property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Legacy,
		"each pad starts with its own Legacy default"
	);

	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad), "the CAPS event is accepted");
	assert!(send_caps(&legacy_pad), "the second pad accepts CAPS independently");
	assert_eq!(
		child.property::<String>("track"),
		"camera",
		"the pad reads back the name its producer reserved"
	);

	child.set_property("track", "other");
	child.set_property("container", gstmoq::MediaContainer::Legacy);
	legacy_child.set_property("container", gstmoq::MediaContainer::Loc);
	assert_eq!(
		child.property::<String>("track"),
		"camera",
		"a write after the reservation is ignored, not stored"
	);
	assert_eq!(
		child.property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Loc,
		"the producer keeps the container captured at CAPS"
	);
	assert_eq!(
		legacy_child.property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Legacy,
		"the second producer keeps its independent container"
	);

	sink.set_state(gst::State::Ready)
		.expect("Paused -> Ready stops the session");
	child.set_property("track", "other");
	child.set_property("container", gstmoq::MediaContainer::Legacy);
	legacy_child.set_property("container", gstmoq::MediaContainer::Loc);
	assert_eq!(
		child.property::<String>("track"),
		"other",
		"a stopped element is configurable again"
	);
	assert_eq!(
		child.property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Legacy
	);
	assert_eq!(
		legacy_child.property::<gstmoq::MediaContainer>("container"),
		gstmoq::MediaContainer::Loc
	);
	let _ = sink.set_state(gst::State::Null);
}

// An unnamed pad reports the generated name, so `track` tells a pad that reserved a track apart from
// one that never received CAPS without going to the consumer.
#[test]
fn an_unnamed_pad_reads_back_its_generated_name() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	assert_eq!(
		child.property::<Option<String>>("track"),
		None,
		"nothing is reserved before CAPS"
	);

	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad), "the CAPS event is accepted");
	assert_eq!(child.property::<String>("track"), "0.avc3");
	let _ = sink.set_state(gst::State::Null);
}

// Releasing an active pad emits notify::track after its producer is removed but before GStreamer
// detaches the pad. Re-enter CAPS handling from that exact window: it must be rejected, and a new pad
// with the same GStreamer name must build a fresh producer instead of finding a ghost one in the map.
#[test]
fn release_rejects_pad_traffic_before_detach() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad), "the initial CAPS event is accepted");
	assert_eq!(child.property::<Option<String>>("track").as_deref(), Some("0.avc3"));

	let attempted = Arc::new(AtomicBool::new(false));
	let caps_accepted = Arc::new(AtomicBool::new(false));
	let eos_accepted = Arc::new(AtomicBool::new(false));
	let buffer_accepted = Arc::new(AtomicBool::new(false));
	let attempted_notify = attempted.clone();
	let caps_notify = caps_accepted.clone();
	let eos_notify = eos_accepted.clone();
	let buffer_notify = buffer_accepted.clone();
	let pad_notify = pad.clone();
	child.connect_notify(Some("track"), move |_, _| {
		if !attempted_notify.swap(true, Ordering::SeqCst) {
			caps_notify.store(
				pad_notify.send_event(gst::event::Caps::new(&h264_caps())),
				Ordering::SeqCst,
			);
			eos_notify.store(pad_notify.send_event(gst::event::Eos::new()), Ordering::SeqCst);
			buffer_notify.store(pad_notify.chain(gst::Buffer::new()).is_ok(), Ordering::SeqCst);
		}
	});

	sink.release_request_pad(&pad);
	assert!(
		attempted.load(Ordering::SeqCst),
		"release reached the vulnerable notify window"
	);
	assert!(
		!caps_accepted.load(Ordering::SeqCst),
		"CAPS was rejected once release began"
	);
	assert!(
		!eos_accepted.load(Ordering::SeqCst),
		"EOS was rejected once release began"
	);
	assert!(
		!buffer_accepted.load(Ordering::SeqCst),
		"a buffer was rejected once release began"
	);

	let replacement = sink.request_pad_simple("sink_0").expect("request replacement sink_0");
	assert!(send_caps(&replacement), "the replacement CAPS event is accepted");
	let replacement_track = child_of(&sink, "sink_0").property::<Option<String>>("track");
	let _ = sink.set_state(gst::State::Null);
	assert!(
		replacement_track.is_some(),
		"the removed pad left no ghost producer under sink_0"
	);
}

// The template announces opaque data, so a data branch negotiates instead of failing to link.
#[test]
fn the_pad_template_announces_opaque_data() {
	init();
	let sink = gst::ElementFactory::make("moqsink").build().expect("create moqsink");
	let template = sink
		.element_class()
		.pad_template("sink_%u")
		.expect("the sink_%u template");
	assert!(
		template
			.caps()
			.can_intersect(&gst::Caps::builder("application/octet-stream").build()),
		"the template announces application/octet-stream"
	);
}

// The CAPS event is the synchronous gate: an unsupported type is refused there, not published.
#[test]
fn unsupported_caps_are_refused_at_the_caps_event() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	child.set_property("track", "audiolevels");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(pad.send_event(gst::event::StreamStart::new("test")));

	for refused in ["application/json", "video/x-raw"] {
		let caps = gst::Caps::builder(refused).build();
		assert!(
			!pad.send_event(gst::event::Caps::new(&caps)),
			"{refused} is refused at the CAPS event"
		);
	}
	let caps = gst::Caps::builder("application/octet-stream").build();
	assert!(
		pad.send_event(gst::event::Caps::new(&caps)),
		"opaque data passes the same gate"
	);
	child.set_property("track", "other");
	assert_eq!(
		child.property::<String>("track"),
		"audiolevels",
		"the CAPS event committed the effective track and made it immutable"
	);
	let _ = sink.set_state(gst::State::Null);
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
