#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	moq_net::fuzz::lite_wire(data);
});
