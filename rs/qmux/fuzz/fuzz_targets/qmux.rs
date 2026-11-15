#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOCATOR: qmux::fuzz::Counting = qmux::fuzz::Counting;

fuzz_target!(|data: &[u8]| {
	qmux::fuzz::qmux(data);
});
