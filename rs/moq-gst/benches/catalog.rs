//! Per-pad EOS work across total renditions x touched pads, while the first catalog is gated.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use gst::prelude::*;

fn eos(c: &mut Criterion) {
	gst::init().unwrap();
	gstmoq::plugin_register_static().unwrap();
	let caps = gst::Caps::builder("audio/x-opus")
		.field("rate", 48_000i32)
		.field("channels", 2i32)
		.build();
	let mut group = c.benchmark_group("moqsink/eos");
	for renditions in [1, 16, 64] {
		let sink = gst::ElementFactory::make("moqsink")
			.property("url", "tcp://127.0.0.1:1")
			.property("broadcast", "bench")
			.build()
			.unwrap();
		let pads = (0..renditions)
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
		for touched in [1, 8, 64] {
			group.throughput(Throughput::Elements(touched));
			group.bench_function(BenchmarkId::new(format!("renditions={renditions}"), touched), |b| {
				b.iter(|| {
					for index in 0..touched as usize {
						let pad = &pads[index % pads.len()];
						assert!(pad.send_event(gst::event::FlushStart::new()));
						assert!(pad.send_event(gst::event::FlushStop::new(false)));
						assert!(pad.send_event(gst::event::Eos::new()));
					}
				});
			});
		}
		sink.set_state(gst::State::Null).unwrap();
	}
	group.finish();
}

criterion_group!(benches, eos);
criterion_main!(benches);
