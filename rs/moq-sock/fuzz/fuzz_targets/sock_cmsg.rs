#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	moq_sock::udp::fuzz::sock_cmsg(data);
});
