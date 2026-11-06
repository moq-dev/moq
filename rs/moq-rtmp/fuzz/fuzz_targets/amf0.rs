#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOCATOR: moq_rtmp::fuzz::Counting = moq_rtmp::fuzz::Counting;

fuzz_target!(|data: &[u8]| {
	moq_rtmp::fuzz::amf0(data);
});
