#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	moq_uring::udp::fuzz::uring_cmsg(data);
});
