//! Hermetic element-boundary tests: behaviour reachable without a live MoQ session.
//!
//! Flows that need a connected session (multipad EOS aggregation, per-pad error propagation, remote
//! close) are validated against a real relay, separately from this hermetic suite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

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

/// The `status` nick, read the way an application without this crate's types would: through the enum
/// class rather than by naming the Rust enum.
fn status_of(sink: &gst::Element, pad: &str) -> String {
	let value = child_of(sink, pad).property_value("track-status");
	let (_, variant) = gst::glib::EnumValue::from_value(&value).expect("status is an enum property");
	variant.nick().to_string()
}

fn track_error_of(sink: &gst::Element, pad: &str) -> Option<String> {
	child_of(sink, pad).property::<Option<String>>("track-error")
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

fn state_change_blocker(fail_when_active: bool) -> (gst::Pad, Arc<AtomicBool>) {
	let enabled = Arc::new(AtomicBool::new(true));
	let fail = enabled.clone();
	let pad = gst::Pad::builder(gst::PadDirection::Sink)
		.name("state-change-blocker")
		.activatemode_function(move |_, _, _, active| {
			if fail.load(Ordering::SeqCst) && active == fail_when_active {
				return Err(gst::loggable_error!(gst::CAT_DEFAULT, "forced state-change failure"));
			}
			Ok(())
		})
		.build();
	(pad, enabled)
}

fn assert_pad_has_no_live_publication(pad: &gst::Pad) {
	pad.set_active(true).expect("activate the pad for the probe");
	assert_eq!(
		pad.chain(gst::Buffer::new()),
		Err(gst::FlowError::Flushing),
		"the failed transition left no publication attached to the pad"
	);
	pad.set_active(false).expect("deactivate the probe pad");
}

fn h264_caps() -> gst::Caps {
	gst::Caps::builder("video/x-h264")
		.field("stream-format", "byte-stream")
		.field("alignment", "au")
		.build()
}

fn unsupported_caps() -> gst::Caps {
	gst::Caps::builder("video/x-raw").build()
}

fn request_unrestricted_sink_pad(sink: &gst::Element) -> gst::Pad {
	let template = gst::PadTemplate::builder(
		"sink_%u",
		gst::PadDirection::Sink,
		gst::PadPresence::Request,
		&gst::Caps::new_any(),
	)
	.build()
	.expect("build unrestricted request-pad template");
	sink.request_pad(&template, Some("sink_0"), None)
		.expect("request unrestricted sink_0")
}

fn h264_keyframe() -> gst::Buffer {
	let sps: &[u8] = &[
		0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
		0x01, 0xd4, 0xc0, 0x80,
	];
	let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
	let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x21];
	let mut au = Vec::new();
	for nal in [sps, pps, idr] {
		au.extend_from_slice(&[0, 0, 0, 1]);
		au.extend_from_slice(nal);
	}
	let mut buffer = gst::Buffer::from_mut_slice(au);
	buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
	buffer
}

/// Drive one pad to the point where it reserves its track: STREAM_START keeps the sticky events in
/// order, CAPS builds the producer.
fn send_caps(pad: &gst::Pad) -> bool {
	pad.send_event(gst::event::StreamStart::new("test")) && pad.send_event(gst::event::Caps::new(&h264_caps()))
}

/// Collect every message the element posts, so a test can assert what reached the bus and what did not.
fn recording_bus(sink: &gst::Element) -> Arc<Mutex<Vec<gst::MessageType>>> {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	let bus = gst::Bus::new();
	bus.set_sync_handler(move |_, message| {
		recorder.lock().unwrap().push(message.type_());
		gst::BusSyncReply::Drop
	});
	sink.set_bus(Some(&bus));
	seen
}

fn posted_eos(seen: &Arc<Mutex<Vec<gst::MessageType>>>) -> usize {
	seen.lock()
		.unwrap()
		.iter()
		.filter(|kind| **kind == gst::MessageType::Eos)
		.count()
}

// READY -> PAUSED is synchronous: the element creates its session without waiting for a buffer, which
// is the preroll policy it inherits from deriving on GstElement rather than a sink base class.
#[test]
fn ready_to_paused_completes_without_a_buffer() {
	init();
	let sink = publisher();
	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(
		sink.set_state(gst::State::Paused),
		Ok(gst::StateChangeSuccess::Success),
		"the transition completed rather than going async"
	);
	let _ = sink.set_state(gst::State::Null);
}

#[test]
fn a_failed_ready_to_paused_rolls_back_the_publication() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let (blocker, enabled) = state_change_blocker(true);
	sink.add_pad(&blocker).expect("add activation blocker");

	assert!(
		sink.set_state(gst::State::Paused).is_err(),
		"the parent transition reached the controlled activation failure"
	);
	assert_pad_has_no_live_publication(&pad);

	enabled.store(false, Ordering::SeqCst);
	let _ = sink.set_state(gst::State::Null);
}

// A sink posts EOS only in PLAYING. Reaching it in PAUSED still finalizes the pad; the message waits.
#[test]
fn eos_in_paused_finalizes_but_holds_the_message() {
	init();
	let sink = publisher();
	let seen = recording_bus(&sink);
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the session");
	assert!(send_caps(&pad));

	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(status_of(&sink, "sink_0"), "ended", "the pad finalized in PAUSED");
	assert_eq!(posted_eos(&seen), 0, "the message waits for PLAYING");

	sink.set_state(gst::State::Playing).expect("Paused -> Playing");
	assert_eq!(posted_eos(&seen), 1, "entering PLAYING released it");
	let _ = sink.set_state(gst::State::Null);
}

// One EOS per publication, not one per entry to PLAYING. Re-posting exists for a player that pauses at
// the end of a file and resumes without seeking; here the publication is already consumed and the only
// way back is a cycle through READY, so a second announcement would say nothing new.
#[test]
fn the_eos_is_posted_once_per_publication() {
	init();
	let sink = publisher();
	let seen = recording_bus(&sink);
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(posted_eos(&seen), 1);

	sink.set_state(gst::State::Paused).expect("Playing -> Paused");
	sink.set_state(gst::State::Playing).expect("Paused -> Playing");
	assert_eq!(posted_eos(&seen), 1, "the second entry adds nothing");
	let _ = sink.set_state(gst::State::Null);
}

// After the publication ends, a buffer that got past the pad's own EOS flag (a flush clears it) must
// report the end rather than be dropped as if it had been published.
#[test]
fn a_buffer_after_the_end_reports_eos_instead_of_ok() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
	segment.set_start(gst::ClockTime::ZERO);
	assert!(pad.send_event(gst::event::Segment::new(&segment)));
	assert!(pad.send_event(gst::event::Eos::new()));

	// FLUSH_STOP clears the pad's EOS in the core, so the buffer reaches the chain function.
	assert!(pad.send_event(gst::event::FlushStart::new()));
	assert!(pad.send_event(gst::event::FlushStop::new(true)));
	assert_eq!(
		pad.chain(h264_keyframe()),
		Err(gst::FlowError::Eos),
		"the publication ended, so the buffer is refused rather than silently dropped"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A SEGMENT after the end must not cost the pad its link. Losing it answered the next buffer with
// Flushing, which reads as "this pad is going away" rather than "the publication is over".
#[test]
fn a_segment_after_the_end_keeps_the_terminal_result() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
	segment.set_start(gst::ClockTime::ZERO);
	assert!(pad.send_event(gst::event::Segment::new(&segment)));
	assert!(pad.send_event(gst::event::Eos::new()));

	// The usual restart sequence: flush, then a fresh segment, then data.
	assert!(pad.send_event(gst::event::FlushStart::new()));
	assert!(pad.send_event(gst::event::FlushStop::new(true)));
	assert!(pad.send_event(gst::event::Segment::new(&segment)));
	assert_eq!(
		pad.chain(h264_keyframe()),
		Err(gst::FlowError::Eos),
		"the publication ended, and a new segment does not change that"
	);
	let _ = sink.set_state(gst::State::Null);
}

// The authorization is the claim, taken while the element was playing. A handler that drops the
// element to PAUSED between the notifications and the post does not revoke it: holding anything across
// post_message() to make it revocable is the re-entrancy this element avoids by design.
#[test]
fn an_eos_claimed_while_playing_survives_a_handler_that_pauses() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));

	let pause_succeeded = Arc::new(AtomicBool::new(false));
	let eos_after_pause = Arc::new(AtomicBool::new(false));
	let observed = eos_after_pause.clone();
	let paused_before_eos = pause_succeeded.clone();
	let bus = gst::Bus::new();
	bus.set_sync_handler(move |_, message| {
		if message.type_() == gst::MessageType::Eos {
			observed.store(paused_before_eos.load(Ordering::SeqCst), Ordering::SeqCst);
		}
		gst::BusSyncReply::Drop
	});
	sink.set_bus(Some(&bus));

	let paused = sink.clone();
	let transition = pause_succeeded.clone();
	child_of(&sink, "sink_0").connect_notify(Some("track-status"), move |_, _| {
		if status_of(&paused, "sink_0") == "ended" {
			transition.store(
				paused.set_state(gst::State::Paused) == Ok(gst::StateChangeSuccess::Success),
				Ordering::SeqCst,
			);
		}
	});

	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(
		sink.current_state(),
		gst::State::Paused,
		"the notify handler completed the downward transition"
	);
	assert!(
		eos_after_pause.load(Ordering::SeqCst),
		"the EOS was posted only after that transition completed"
	);
	let _ = sink.set_state(gst::State::Null);
}

// Leaving PLAYING closes the gate. Without that arm an EOS reached in PAUSED would go out immediately,
// which is the rule the whole retention exists to keep.
#[test]
fn leaving_playing_holds_a_later_eos_until_the_next_entry() {
	init();
	let sink = publisher();
	let seen = recording_bus(&sink);
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	sink.set_state(gst::State::Paused).expect("Playing -> Paused");

	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(posted_eos(&seen), 0, "the gate closed on the way down");

	sink.set_state(gst::State::Playing).expect("Paused -> Playing");
	assert_eq!(posted_eos(&seen), 1, "and the next entry delivers it once");
	let _ = sink.set_state(gst::State::Null);
}

// The publication does not reopen: recovery is a cycle through READY, not a late pad.
#[test]
fn a_pad_requested_after_the_end_is_refused() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));

	assert!(
		sink.request_pad_simple("sink_1").is_none(),
		"a finished publication admits no new pad"
	);
	sink.set_state(gst::State::Ready).expect("Playing -> Ready");
	sink.set_state(gst::State::Paused).expect("Ready -> Paused");
	assert!(
		sink.request_pad_simple("sink_1").is_some(),
		"a new run admits pads again"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A failed parent leaves the element in PAUSED, so the session stays with it. Tearing it down would
// leave a PAUSED element publishing nothing and no transition left to build a replacement.
#[test]
fn a_failed_paused_to_ready_keeps_the_publication() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let (blocker, enabled) = state_change_blocker(false);
	sink.add_pad(&blocker).expect("add deactivation blocker");
	sink.set_state(gst::State::Paused).expect("start the publication");
	pad.set_active(true).expect("activate the pad");
	assert!(send_caps(&pad));

	assert!(
		sink.set_state(gst::State::Ready).is_err(),
		"the parent transition reached the controlled deactivation failure"
	);
	assert!(
		pad.property::<Option<String>>("track").is_some(),
		"the element is still in PAUSED, so its publication is still live"
	);

	// The retry succeeds and is what releases it.
	enabled.store(false, Ordering::SeqCst);
	sink.set_state(gst::State::Ready).expect("the retry completes");
	assert_eq!(
		pad.property::<Option<String>>("track"),
		None,
		"reaching READY released the reservation"
	);
	let _ = sink.set_state(gst::State::Null);
}
// The teardown ordering: the parent is what deactivates the pads and waits for the streaming functions
// to return, so the publication has to outlive that. Finalizing first lets a chain function still in
// flight write into a finalized producer. Observed from inside pad deactivation, where a streaming
// function would be.
#[test]
fn the_publication_outlives_pad_deactivation() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the publication");
	pad.set_active(true).expect("activate the pad");
	assert!(send_caps(&pad));
	let reserved = pad.property::<Option<String>>("track").expect("CAPS reserved a track");

	let seen = Arc::new(Mutex::new(None));
	let record = seen.clone();
	let observed = pad.clone();
	let probe = gst::Pad::builder(gst::PadDirection::Sink)
		.name("deactivation-probe")
		.activatemode_function(move |_, _, _, active| {
			if !active {
				record
					.lock()
					.unwrap()
					.replace(observed.property::<Option<String>>("track"));
			}
			Ok(())
		})
		.build();
	sink.add_pad(&probe).expect("add the probe");
	probe.set_active(true).expect("activate the probe");

	sink.set_state(gst::State::Ready).expect("tear down");
	assert_eq!(
		seen.lock().unwrap().clone().flatten().as_deref(),
		Some(reserved.as_str()),
		"the publication was still live while the pads were being deactivated"
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

// The CAPS event is the synchronous gate: caps admitted by the template but lacking required codec
// configuration are refused there, not published.
#[test]
fn invalid_caps_are_refused_at_the_caps_event() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	child.set_property("track", "audiolevels");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(pad.send_event(gst::event::StreamStart::new("test")));

	let opaque = gst::Caps::builder("application/octet-stream").build();
	assert!(pad.send_event(gst::event::Caps::new(&opaque)));
	assert_eq!(status_of(&sink, "sink_0"), "active");
	child.set_property("track", "other");
	assert_eq!(
		child.property::<String>("track"),
		"audiolevels",
		"the CAPS event committed the effective track and made it immutable"
	);

	let invalid = gst::Caps::builder("audio/mpeg")
		.field("mpegversion", 4i32)
		.field("stream-format", "raw")
		.build();
	assert!(!pad.send_event(gst::event::Caps::new(&invalid)));
	assert_eq!(status_of(&sink, "sink_0"), "error");
	assert!(track_error_of(&sink, "sink_0").is_some());

	assert!(pad.send_event(gst::event::Caps::new(&h264_caps())));
	assert_eq!(status_of(&sink, "sink_0"), "error", "a failed pad stays terminal");
	assert_eq!(
		child.property::<String>("track"),
		"audiolevels",
		"the requested name remains visible while the failed pad is retained"
	);
	let _ = sink.set_state(gst::State::Null);
}

// Types neither the media importer nor the opaque-data path supports fail synchronously at CAPS.
#[test]
fn unsupported_caps_are_refused_at_the_caps_event() {
	init();
	// Through an unrestricted pad, so the refusal comes from the element and not from the template
	// filtering the caps out before the event function ever runs. Asserting the pad's status as well as
	// the return value is what tells the two apart.
	for refused in ["application/json", "video/x-raw"] {
		let sink = publisher();
		let pad = request_unrestricted_sink_pad(&sink);
		sink.set_state(gst::State::Paused)
			.expect("Ready -> Paused starts the session");
		assert!(pad.send_event(gst::event::StreamStart::new("test")));
		let caps = gst::Caps::builder(refused).build();
		assert!(
			!pad.send_event(gst::event::Caps::new(&caps)),
			"{refused} is refused at the CAPS event"
		);
		assert_eq!(
			status_of(&sink, "sink_0"),
			"error",
			"{refused} reached the element and failed the pad"
		);
		let _ = sink.set_state(gst::State::Null);
	}
}

#[test]
fn unsupported_caps_mark_the_pad_unless_the_publication_is_terminal() {
	init();

	let sink = publisher();
	let pad = request_unrestricted_sink_pad(&sink);
	sink.set_state(gst::State::Ready)
		.expect("enter READY without starting a session");
	pad.set_active(true).expect("activate the pad without a session");
	assert!(pad.send_event(gst::event::StreamStart::new("no-session")));
	assert!(!pad.send_event(gst::event::Caps::new(&unsupported_caps())));
	assert_eq!(
		status_of(&sink, "sink_0"),
		"error",
		"no session still records the pad error"
	);
	pad.set_active(false).expect("deactivate the pad");
	let _ = sink.set_state(gst::State::Null);

	let sink = publisher();
	let pad = request_unrestricted_sink_pad(&sink);
	sink.set_state(gst::State::Paused).expect("start the publication");
	assert!(pad.send_event(gst::event::StreamStart::new("open")));
	assert!(!pad.send_event(gst::event::Caps::new(&unsupported_caps())));
	assert_eq!(
		status_of(&sink, "sink_0"),
		"error",
		"an open publication records the pad error"
	);
	let _ = sink.set_state(gst::State::Null);

	let sink = publisher();
	let pad = request_unrestricted_sink_pad(&sink);
	sink.set_state(gst::State::Playing).expect("start playing");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(status_of(&sink, "sink_0"), "ended");
	assert!(pad.send_event(gst::event::StreamStart::new("terminal")));
	assert!(!pad.send_event(gst::event::Caps::new(&unsupported_caps())));
	assert_eq!(
		status_of(&sink, "sink_0"),
		"ended",
		"late CAPS do not rewrite a terminal pad"
	);
	assert_eq!(track_error_of(&sink, "sink_0"), None);
	let _ = sink.set_state(gst::State::Null);
}

// The acceptance criterion: a failed pad names its own reason, and the failure is isolated to it.
#[test]
fn an_unnamed_opaque_pad_reports_its_error() {
	init();
	let sink = publisher();
	let data = sink.request_pad_simple("sink_0").expect("request sink_0");
	let video = sink.request_pad_simple("sink_1").expect("request sink_1");
	child_of(&sink, "sink_1").set_property("track", "camera");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");

	assert!(video.send_event(gst::event::StreamStart::new("video")));
	assert!(send_caps(&video), "the video CAPS event is accepted");
	assert!(data.send_event(gst::event::StreamStart::new("data")));
	let opaque = gst::Caps::builder("application/octet-stream").build();
	assert!(
		!data.send_event(gst::event::Caps::new(&opaque)),
		"the missing required track name rejects the CAPS event"
	);

	assert_eq!(status_of(&sink, "sink_0"), "error");
	assert_eq!(
		track_error_of(&sink, "sink_0").as_deref(),
		Some("an opaque data pad requires a track name")
	);
	assert_eq!(status_of(&sink, "sink_1"), "active", "the video pad is untouched");
	assert_eq!(track_error_of(&sink, "sink_1"), None);
	let _ = sink.set_state(gst::State::Null);
}

// No timeout invents a failure: a pad with no CAPS has not failed, it has not started.
#[test]
fn a_pad_without_caps_stays_pending() {
	init();
	let sink = publisher();
	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(status_of(&sink, "sink_0"), "pending");

	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert_eq!(status_of(&sink, "sink_0"), "pending", "starting is not publishing");
	assert_eq!(track_error_of(&sink, "sink_0"), None);
	let _ = sink.set_state(gst::State::Null);
}

// A CAPS event that reaches no catalog built no producer and rejected nothing, so it must not move
// the status. Renegotiating after EOS is the reachable form of that: the catalog is already gone.
#[test]
fn caps_with_no_catalog_left_does_not_move_the_status() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad), "the CAPS event is accepted");
	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(status_of(&sink, "sink_0"), "ended");

	let renegotiated = gst::Caps::builder("video/x-h264")
		.field("stream-format", "byte-stream")
		.field("alignment", "au")
		.field("width", 1280i32)
		.build();
	// STREAM_START first: the EOS flag makes the pad refuse serialized events, so without it the CAPS
	// event never reaches the handler and the assertion below would pass on inertia.
	assert!(pad.send_event(gst::event::StreamStart::new("restart")));
	assert!(pad.send_event(gst::event::Caps::new(&renegotiated)));
	assert_eq!(
		status_of(&sink, "sink_0"),
		"ended",
		"no catalog means no outcome to report, not a failure"
	);
	assert_eq!(track_error_of(&sink, "sink_0"), None);
	let _ = sink.set_state(gst::State::Null);
}

// A name another pad holds invalidates only the second one, and says so.
#[test]
fn a_colliding_name_reports_on_the_second_pad() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	child_of(&sink, "sink_0").set_property("track", "camera");
	child_of(&sink, "sink_1").set_property("track", "camera");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");

	assert!(send_caps(&first));
	assert!(second.send_event(gst::event::StreamStart::new("second")));
	assert!(!send_caps(&second), "the duplicate track name rejects the CAPS event");

	assert_eq!(status_of(&sink, "sink_0"), "active");
	assert_eq!(status_of(&sink, "sink_1"), "error");
	assert!(
		track_error_of(&sink, "sink_1")
			.expect("the second pad names its failure")
			.contains("camera"),
		"the reason names the track it could not reserve"
	);
	let _ = sink.set_state(gst::State::Null);
}

// EOS ends the track only when the producer is actually finalized, which is once every pad ended.
#[test]
fn eos_ends_a_pad_only_once_every_pad_ended() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	child_of(&sink, "sink_0").set_property("track", "camera");
	child_of(&sink, "sink_1").set_property("track", "commentary");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&first));
	assert!(second.send_event(gst::event::StreamStart::new("second")));
	assert!(send_caps(&second));

	assert!(first.send_event(gst::event::Eos::new()));
	assert_eq!(
		status_of(&sink, "sink_0"),
		"active",
		"its track is still open while the other pad runs"
	);

	assert!(second.send_event(gst::event::Eos::new()));
	assert_eq!(status_of(&sink, "sink_0"), "ended");
	assert_eq!(status_of(&sink, "sink_1"), "ended");
	let _ = sink.set_state(gst::State::Null);
}

// The first notify of the finalize pass runs application code, which may release a pad and request
// the same name again. The session is already finalized by then, so the request is refused.
#[test]
fn a_pad_requested_from_the_eos_notify_is_refused() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&first));
	assert!(second.send_event(gst::event::StreamStart::new("second")));
	assert!(send_caps(&second));
	assert!(second.send_event(gst::event::Eos::new()));

	// Swap sink_1 out from under the pass, from inside the first status notify it emits.
	let swapped = Arc::new(AtomicBool::new(false));
	let swap = swapped.clone();
	let element = sink.clone();
	let old = second.clone();
	child_of(&sink, "sink_0").connect_notify(Some("track-status"), move |_, _| {
		if swap.swap(true, Ordering::SeqCst) {
			return;
		}
		element.release_request_pad(&old);
		assert!(element.request_pad_simple("sink_1").is_none());
	});

	assert!(first.send_event(gst::event::Eos::new()));
	assert!(swapped.load(Ordering::SeqCst), "the swap ran inside the notify");
	assert_eq!(sink.num_sink_pads(), 1, "the refused pad left nothing attached");
	// The object the application kept: released is released, and a result from the life it just left
	// must not be written back onto it.
	let released = second
		.downcast_ref::<gst::Pad>()
		.expect("the released pad is still held");
	let value = released.property_value("track-status");
	let (_, variant) = gst::glib::EnumValue::from_value(&value).expect("status is an enum property");
	assert_eq!(
		variant.nick(),
		"pending",
		"the released pad kept its reset instead of taking a stale ended"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A write that the producer rejects is the failure the property exists for. A frame past
// MAX_CACHE_BYTES is the deterministic way to cause one: moq-net refuses it before reserving a group.
#[test]
fn a_rejected_write_moves_the_pad_to_error() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	child.set_property("track", "audiolevels");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");

	assert!(pad.send_event(gst::event::StreamStart::new("data")));
	let opaque = gst::Caps::builder("application/octet-stream").build();
	assert!(pad.send_event(gst::event::Caps::new(&opaque)));
	let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
	segment.set_start(gst::ClockTime::ZERO);
	assert!(pad.send_event(gst::event::Segment::new(&segment)));
	assert_eq!(status_of(&sink, "sink_0"), "active");

	let notifies = Arc::new(Mutex::new(Vec::new()));
	let recorder = notifies.clone();
	child.connect_notify(None, move |_, spec| {
		recorder.lock().unwrap().push(spec.name().to_string())
	});

	// One byte past the cache ceiling, so the producer rejects the frame instead of storing it.
	let mut buffer = gst::Buffer::with_size(32 * 1024 * 1024 + 1).expect("allocate an oversized buffer");
	buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
	assert!(pad.chain(buffer).is_ok(), "a per-pad failure is not a flow error");

	assert_eq!(status_of(&sink, "sink_0"), "error");
	assert_eq!(track_error_of(&sink, "sink_0").as_deref(), Some("frame too large"));
	let notifies = notifies.lock().unwrap();
	assert_eq!(
		notifies.iter().filter(|n| *n == "track-status").count(),
		1,
		"the move to error is announced once"
	);
	assert_eq!(notifies.iter().filter(|n| *n == "track-error").count(), 1);
	drop(notifies);
	let _ = sink.set_state(gst::State::Null);
}

// A bus sync handler runs on the thread that posts, and reading `status` there is the point of the
// property. The EOS message must therefore go out with no pad lock held, and with the status already
// settled: this hangs if the finalize pass posts while it still holds the pad it is reporting on.
#[test]
fn a_sync_bus_handler_can_read_the_status_on_eos() {
	init();
	let sink = publisher();
	let bus = gst::Bus::new();
	sink.set_bus(Some(&bus));
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad));
	// A sink posts EOS only in PLAYING, so the handler under test needs the element there to run at all.
	sink.set_state(gst::State::Playing).expect("Paused -> Playing");

	let seen = Arc::new(Mutex::new(None));
	let recorder = seen.clone();
	let element = sink.clone();
	bus.set_sync_handler(move |_, message| {
		if message.type_() == gst::MessageType::Eos {
			*recorder.lock().unwrap() = Some(status_of(&element, "sink_0"));
		}
		gst::BusSyncReply::Drop
	});

	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(
		seen.lock().unwrap().as_deref(),
		Some("ended"),
		"the handler read the settled status instead of deadlocking on it"
	);
	let _ = sink.set_state(gst::State::Null);
}

// Finalization settles the pad before notifying it. A handler can therefore stop that run and start
// another before the deferred EOS reaches the bus; the old run must not complete its replacement.
#[test]
fn replacing_the_session_from_eos_notify_discards_the_old_eos() {
	init();
	let sink = publisher();
	let bus = gst::Bus::new();
	sink.set_bus(Some(&bus));
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let restarted = Arc::new(AtomicBool::new(false));
	let done = restarted.clone();
	let element = sink.clone();
	child_of(&sink, "sink_0").connect_notify(Some("track-status"), move |pad, _| {
		let value = pad.property_value("track-status");
		let (_, status) = gst::glib::EnumValue::from_value(&value).expect("status enum");
		if status.nick() == "ended" && !done.swap(true, Ordering::SeqCst) {
			element
				.set_state(gst::State::Ready)
				.expect("stop the completed session");
			element.set_state(gst::State::Paused).expect("start its replacement");
		}
	});

	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad));
	while bus.pop().is_some() {}

	assert!(pad.send_event(gst::event::Eos::new()));
	assert!(restarted.load(Ordering::SeqCst));
	assert!(
		bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Eos])
			.is_none(),
		"the completed session posted EOS into its replacement"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A pad that ends and then starts a new stream is publishing again. Leaving it counted as ended would
// let the next pad EOS finalize the whole element, cutting a track that is still live.
#[test]
fn a_restarted_pad_is_no_longer_counted_as_ended() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	let second = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&first));
	assert!(second.send_event(gst::event::StreamStart::new("second")));
	assert!(send_caps(&second));

	assert!(first.send_event(gst::event::Eos::new()));
	// sink_0 comes back with a new stream before sink_1 ever ends.
	assert!(first.send_event(gst::event::StreamStart::new("restart")));
	assert!(second.send_event(gst::event::Eos::new()));

	assert_eq!(
		status_of(&sink, "sink_0"),
		"active",
		"a restarted pad is publishing, so nothing finalized it"
	);
	let _ = sink.set_state(gst::State::Null);
}

// A live request pad accepts data before CAPS and drops it locally. Once released, GStreamer rejects
// the same operation because release deactivated the pad.
#[test]
fn a_registered_pad_takes_data_before_caps() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the session");
	assert!(pad.send_event(gst::event::StreamStart::new("test")));
	let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
	segment.set_start(gst::ClockTime::ZERO);
	assert!(pad.send_event(gst::event::Segment::new(&segment)));

	let mut buffer = gst::Buffer::with_size(4).expect("allocate buffer");
	buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
	assert_eq!(pad.chain(buffer), Ok(gst::FlowSuccess::Ok));
	assert_eq!(status_of(&sink, "sink_0"), "pending");

	sink.release_request_pad(&pad);
	let mut buffer = gst::Buffer::with_size(4).expect("allocate buffer");
	buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
	assert_eq!(pad.chain(buffer), Err(gst::FlowError::Flushing));
	let _ = sink.set_state(gst::State::Null);
}

// SEGMENT can arrive from pad-added before request_new_pad confirms the admission. It must bind the
// new member to the live session so the following pre-CAPS buffer is dropped locally, not flushed.
#[test]
fn a_segment_from_pad_added_binds_the_live_session() {
	init();
	let sink = publisher();
	sink.set_state(gst::State::Paused).expect("start the session");
	let accepted = Arc::new(AtomicBool::new(false));
	let result = accepted.clone();
	sink.connect_pad_added(move |_, pad| {
		assert!(pad.send_event(gst::event::StreamStart::new("test")));
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::ZERO);
		assert!(pad.send_event(gst::event::Segment::new(&segment)));
		let mut buffer = gst::Buffer::with_size(4).expect("allocate buffer");
		buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
		result.store(pad.chain(buffer).is_ok(), Ordering::SeqCst);
	});

	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert!(accepted.load(Ordering::SeqCst));
	let _ = sink.set_state(gst::State::Null);
}

// A pad is a member as soon as GStreamer lists it. EOS delivered from its pad-added callback must not
// finalize past it while request_new_pad is still running.
#[test]
fn a_pad_the_element_already_lists_holds_the_aggregation() {
	init();
	let sink = publisher();
	let ended = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the session");
	assert!(send_caps(&ended));

	let first = ended.clone();
	let signalled = Arc::new(AtomicBool::new(false));
	let done = signalled.clone();
	sink.connect_pad_added(move |_, _| {
		if !done.swap(true, Ordering::SeqCst) {
			assert!(first.send_event(gst::event::Eos::new()));
		}
	});

	let _quiet = sink.request_pad_simple("sink_1").expect("request sink_1");
	assert!(signalled.load(Ordering::SeqCst));
	assert_eq!(status_of(&sink, "sink_0"), "active");
	let _ = sink.set_state(gst::State::Null);
}

// pad-added can negotiate, publish and end before the request returns. The complete lifecycle must
// land on the new pad while its admission is still open.
#[test]
fn a_pad_negotiated_from_pad_added_still_publishes() {
	init();
	let sink = publisher();
	sink.set_state(gst::State::Paused).expect("start the session");

	let published = Arc::new(AtomicBool::new(false));
	let done = published.clone();
	sink.connect_pad_added(move |_, pad| {
		if !send_caps(pad) {
			return;
		}
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::ZERO);
		if !pad.send_event(gst::event::Segment::new(&segment)) {
			return;
		}
		let completed = pad.chain(h264_keyframe()).is_ok() && pad.send_event(gst::event::Eos::new());
		done.store(completed, Ordering::SeqCst);
	});

	let _pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert!(
		published.load(Ordering::SeqCst),
		"the callback published its first buffer"
	);
	assert_eq!(status_of(&sink, "sink_0"), "ended");
	let _ = sink.set_state(gst::State::Null);
}

// EOS from pad-added counts for the new member and is settled before the request returns.
#[test]
fn an_eos_sent_from_pad_added_counts() {
	init();
	let sink = publisher();
	sink.set_state(gst::State::Paused).expect("start the session");
	sink.connect_pad_added(move |_, pad| {
		assert!(pad.send_event(gst::event::StreamStart::new("early")));
		assert!(send_caps(pad));
		assert!(pad.send_event(gst::event::Eos::new()));
	});

	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	assert_eq!(status_of(&sink, "sink_0"), "ended");
	assert_eq!(pad.parent(), Some(sink.clone().upcast()));
	let _ = sink.set_state(gst::State::Null);
}

// Releasing from pad-added detaches the pad before request_new_pad can return it.
#[test]
fn a_pad_released_from_pad_added_is_not_returned() {
	init();
	let sink = publisher();
	let released = Arc::new(AtomicBool::new(false));
	let done = released.clone();
	sink.connect_pad_added(move |element, pad| {
		if !done.swap(true, Ordering::SeqCst) {
			element.release_request_pad(pad);
		}
	});

	assert!(sink.request_pad_simple("sink_0").is_none());
	assert!(released.load(Ordering::SeqCst));
	assert_eq!(sink.num_sink_pads(), 0);
}

// Removing an active pad can make all remaining pads ended, so release must reevaluate aggregation.
#[test]
fn a_released_pad_does_not_hold_back_the_element_eos() {
	init();
	let sink = publisher();
	let kept = sink.request_pad_simple("sink_0").expect("request sink_0");
	let released = sink.request_pad_simple("sink_1").expect("request sink_1");
	sink.set_state(gst::State::Paused).expect("start the session");
	assert!(send_caps(&kept));
	assert!(released.send_event(gst::event::StreamStart::new("second")));
	assert!(send_caps(&released));
	assert!(kept.send_event(gst::event::Eos::new()));

	sink.release_request_pad(&released);
	assert_eq!(status_of(&sink, "sink_0"), "ended");
	let _ = sink.set_state(gst::State::Null);
}

// Release leaves a retained pad inactive, detached and pending.
#[test]
fn release_leaves_the_pad_inactive_and_detached() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start the session");
	assert!(send_caps(&pad));

	sink.release_request_pad(&pad);
	assert!(!pad.is_active());
	assert!(pad.parent().is_none());
	let value = pad.property_value("track-status");
	let (_, status) = gst::glib::EnumValue::from_value(&value).expect("status enum");
	assert_eq!(status.nick(), "pending");
	let _ = sink.set_state(gst::State::Null);
}

// Notify handlers run without lifecycle or control locks, so releasing from the pad's own transition
// returns instead of deadlocking.
#[test]
fn releasing_a_pad_from_its_own_notify_returns() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let released = Arc::new(AtomicBool::new(false));
	let done = released.clone();
	let element = sink.clone();
	let target = pad.clone();
	child_of(&sink, "sink_0").connect_notify(Some("track-status"), move |_, _| {
		if !done.swap(true, Ordering::SeqCst) {
			element.release_request_pad(&target);
		}
	});

	sink.set_state(gst::State::Paused).expect("start the session");
	let _ = send_caps(&pad);
	assert!(released.load(Ordering::SeqCst));
	assert!(!pad.is_active());
	let _ = sink.set_state(gst::State::Null);
}

// A retained request pad is reset at READY and can reserve and publish again in the next run.
#[test]
fn a_pad_kept_across_runs_publishes_again() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start first run");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));
	assert_eq!(status_of(&sink, "sink_0"), "ended");

	sink.set_state(gst::State::Ready).expect("stop first run");
	assert_eq!(status_of(&sink, "sink_0"), "pending");
	sink.set_state(gst::State::Paused).expect("start second run");
	assert!(pad.send_event(gst::event::StreamStart::new("second")));
	assert!(send_caps(&pad));
	let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
	segment.set_start(gst::ClockTime::ZERO);
	assert!(pad.send_event(gst::event::Segment::new(&segment)));
	assert_eq!(pad.chain(h264_keyframe()), Ok(gst::FlowSuccess::Ok));
	assert_eq!(status_of(&sink, "sink_0"), "active");
	let _ = sink.set_state(gst::State::Null);
}

// Final EOS closes admission only for that run. Returning to READY and starting again must accept a
// new request pad instead of retaining eos_posted from the previous session.
#[test]
fn a_new_run_accepts_pads_again() {
	init();
	let sink = publisher();
	let first = sink.request_pad_simple("sink_0").expect("request sink_0");
	sink.set_state(gst::State::Paused).expect("start first run");
	assert!(send_caps(&first));
	assert!(first.send_event(gst::event::Eos::new()));
	assert!(
		sink.request_pad_simple("sink_1").is_none(),
		"the finalized run refuses another pad"
	);

	sink.set_state(gst::State::Ready).expect("stop first run");
	sink.set_state(gst::State::Paused).expect("start second run");
	let second = sink.request_pad_simple("sink_1").expect("the new run accepts sink_1");
	assert_eq!(second.parent(), Some(sink.clone().upcast()));
	let _ = sink.set_state(gst::State::Null);
}

// An application polls the status through notify rather than by asking on a timer.
#[test]
fn status_notifies_on_each_transition() {
	init();
	let sink = publisher();
	let pad = sink.request_pad_simple("sink_0").expect("request sink_0");
	let child = child_of(&sink, "sink_0");
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	child.connect_notify(Some("track-status"), move |obj, _| {
		let value = obj.property_value("track-status");
		let (_, variant) = gst::glib::EnumValue::from_value(&value).expect("status is an enum property");
		recorder.lock().unwrap().push(variant.nick().to_string());
	});

	sink.set_state(gst::State::Paused)
		.expect("Ready -> Paused starts the session");
	assert!(send_caps(&pad));
	assert!(pad.send_event(gst::event::Eos::new()));
	let _ = sink.set_state(gst::State::Null);

	let seen = seen.lock().unwrap();
	assert_eq!(
		seen.as_slice(),
		["active", "ended", "pending"],
		"each move is announced once, including the reset on stop"
	);
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
