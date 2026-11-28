//! Session costs through the model-only relay over the shared in-memory scenarios.
#[path = "session/mod.rs"]
mod session;

use session::{
	Delivering, Fanout, Idle, IdleRoom, Model, RangeUpdates, Running, Span, delivering, fanning, idling, range_updates,
	round, tick,
};
use swole::bench;

session::benches!(Model);

#[bench(
	args = session::delivery_meta(),
	setup = delivering::<Model>,
	before = tick::<Model>,
	bytes = session::Case::bytes
)]
fn session_delivery_meta(room: &mut Delivering<Model>) {
	round(room);
}

#[bench(
	args = session::spans(),
	setup = range_updates::<Model>,
	before = session::narrow::<Model>,
	items = |span: Span| span.requests
)]
fn session_range_updates(fixture: &mut Running<RangeUpdates<Model>>) {
	session::range_update(fixture);
}

#[bench(
	args = session::fanouts(),
	setup = fanning::<Model>,
	before = tick::<Model>,
	items = Fanout::groups
)]
fn session_fanout_threads(room: &mut Delivering<Model>) {
	round(room);
}

#[bench(args = session::idles(), setup = idling, items = |idle: Idle| idle.sessions)]
fn session_idle(room: &mut Running<IdleRoom>) {
	session::idle(room);
}

fn main() {
	swole::main();
}
