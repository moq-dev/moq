//! Per-pad EOS work across total renditions x touched pads, while the first catalog is gated.
//!
//! Run with `cargo bench -p moq-gst --bench catalog`.

use std::fmt;
use std::sync::Once;

use gst::prelude::*;

/// Renditions on the sink and pads each call ends, named `renditions=<n>/<touched>`.
#[derive(Clone, Copy)]
struct Case {
	renditions: usize,
	touched: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "renditions={}/{}", self.renditions, self.touched)
	}
}

fn cases() -> Vec<Case> {
	[1, 16, 64]
		.into_iter()
		.flat_map(|renditions| [1, 8, 64].map(|touched| Case { renditions, touched }))
		.collect()
}

/// A paused `moqsink` with configured pads and one unresolved reservation.
struct Sink {
	sink: gst::Element,
	pads: Vec<gst::Pad>,
	_pending: gst::Pad,
	touched: usize,
}

impl Drop for Sink {
	fn drop(&mut self) {
		self.sink.set_state(gst::State::Null).unwrap();
	}
}

fn sink(case: Case) -> Sink {
	static INIT: Once = Once::new();
	INIT.call_once(|| {
		gst::init().unwrap();
		gstmoq::plugin_register_static().unwrap();
	});
	let caps = gst::Caps::builder("audio/x-opus")
		.field("rate", 48_000i32)
		.field("channels", 2i32)
		.build();
	let sink = gst::ElementFactory::make("moqsink")
		.property("url", "tcp://127.0.0.1:1")
		.property("broadcast", "bench")
		.build()
		.unwrap();
	let pads = (0..case.renditions)
		.map(|_| sink.request_pad_simple("sink_%u").unwrap())
		.collect::<Vec<_>>();
	// Keep one reservation unresolved: the lookup must see current configs without cloning the
	// full catalog or publishing a snapshot, and EOS must not finalize the whole element.
	let _pending = sink.request_pad_simple("sink_%u").unwrap();
	sink.set_state(gst::State::Paused).unwrap();
	for pad in &pads {
		assert!(pad.send_event(gst::event::StreamStart::new(pad.name().as_str())));
		assert!(pad.send_event(gst::event::Caps::new(&caps)));
	}
	Sink {
		sink,
		pads,
		_pending,
		touched: case.touched,
	}
}

mod moqsink_eos {
	use gst::prelude::*;
	use swole::bench;

	use super::{Case, Sink, cases, sink};

	#[bench(args = cases(), setup = sink, items = |case: Case| case.touched)]
	fn eos(sink: &Sink) {
		for index in 0..sink.touched {
			let pad = &sink.pads[index % sink.pads.len()];
			assert!(pad.send_event(gst::event::FlushStart::new()));
			assert!(pad.send_event(gst::event::FlushStop::new(false)));
			assert!(pad.send_event(gst::event::Eos::new()));
		}
	}
}

fn main() {
	swole::main();
}
