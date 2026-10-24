//! Session costs through the model-only relay over the shared in-memory scenarios.
#[path = "session/mod.rs"]
mod session;

use criterion::{criterion_group, criterion_main};
use session::Model;

criterion_group!(
	benches,
	session::session::<Model>,
	session::group_cost::<Model>,
	session::fetch_frames::<Model>,
	session::withdrawal::<Model>,
	session::idle
);
criterion_main!(benches);
