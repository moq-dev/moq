//! Allocation accounting for synchronous receive-path tests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
	static USAGE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

struct Counting;

// All memory operations still use System. Const TLS keeps accounting from
// allocating recursively and isolates concurrent tests on other threads.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc(layout) };
		if !ptr.is_null() {
			let _ = USAGE.try_with(|usage| {
				if let Some((live, peak)) = usage.get() {
					let live = live + layout.size();
					usage.set(Some((live, peak.max(live))));
				}
			});
		}
		ptr
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		let _ = USAGE.try_with(|usage| {
			if let Some((live, peak)) = usage.get() {
				usage.set(Some((live.saturating_sub(layout.size()), peak)));
			}
		});
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

pub fn measure<T>(run: impl FnOnce() -> T) -> (T, usize) {
	USAGE.with(|usage| usage.set(Some((0, 0))));
	let result = run();
	let (_, peak) = USAGE.with(|usage| usage.replace(None).unwrap());
	(result, peak)
}

#[test]
fn sessions_discard_dense_script_data_without_materializing_values() {
	use crate::rml::chunk_io::ChunkSerializer;
	use crate::rml::messages::MessagePayload;
	use crate::rml::sessions::{ClientSession, ClientSessionConfig, ServerSession, ServerSessionConfig};
	for length in [65536, 1048576] {
		for type_id in [15, 18] {
			let payload = MessagePayload {
				type_id,
				data: vec![5; length].into(),
				..MessagePayload::new()
			};
			let packet = ChunkSerializer::new().serialize(&payload, false, false).unwrap();
			let (mut server, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
			let (_, peak) = measure(|| server.handle_input(&packet.bytes).unwrap());
			assert!(
				peak <= 4 * length,
				"listener allocated {peak} bytes for {length} input bytes"
			);
			let (mut client, _) = ClientSession::new(ClientSessionConfig::new()).unwrap();
			let (_, peak) = measure(|| client.handle_input(&packet.bytes).unwrap());
			assert!(
				peak <= 4 * length,
				"dialer allocated {peak} bytes for {length} input bytes"
			);
		}
	}
}
