#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOCATOR: moq_mux::fuzz::Counting = moq_mux::fuzz::Counting;

fuzz_target!(|data: &[u8]| {
	moq_mux::fuzz::run(moq_mux::fuzz::flv, data);
});
