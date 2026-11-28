//! The wire codec on its own: a fixed mix of moq-lite and moq-transport messages, and raw
//! varints in each wire form. Every message and frame header pays this cost.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench codec`.

mod codec_messages {
	use std::fmt;

	use moq_net::fuzz::Messages;
	use swole::bench;

	#[derive(Clone, Copy)]
	enum Protocol {
		Lite,
		Ietf,
	}

	const PROTOCOLS: [Protocol; 2] = [Protocol::Lite, Protocol::Ietf];

	impl Protocol {
		fn encode(self, messages: &Messages, out: &mut Vec<u8>) {
			match self {
				Self::Lite => messages.encode_lite(out),
				Self::Ietf => messages.encode_ietf(out),
			}
		}

		fn wire(self, messages: &Messages) -> Vec<u8> {
			let mut wire = Vec::new();
			self.encode(messages, &mut wire);
			wire
		}

		fn bytes(self) -> usize {
			self.wire(&Messages::default()).len()
		}
	}

	impl fmt::Display for Protocol {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			f.write_str(match self {
				Self::Lite => "lite",
				Self::Ietf => "ietf",
			})
		}
	}

	struct Encoding {
		protocol: Protocol,
		messages: Messages,
		out: Vec<u8>,
	}

	fn encoding(protocol: Protocol) -> Encoding {
		let messages = Messages::default();
		let out = Vec::with_capacity(protocol.wire(&messages).len());
		Encoding {
			protocol,
			messages,
			out,
		}
	}

	#[bench(args = PROTOCOLS, setup = encoding, bytes = Protocol::bytes)]
	fn encode(input: &mut Encoding) {
		input.out.clear();
		input.protocol.encode(&input.messages, &mut input.out);
	}

	struct Decoding {
		protocol: Protocol,
		messages: Messages,
		wire: Vec<u8>,
	}

	fn decoding(protocol: Protocol) -> Decoding {
		let messages = Messages::default();
		let wire = protocol.wire(&messages);
		Decoding {
			protocol,
			messages,
			wire,
		}
	}

	#[bench(args = PROTOCOLS, setup = decoding, bytes = Protocol::bytes)]
	fn decode(input: &Decoding) {
		match input.protocol {
			Protocol::Lite => input.messages.decode_lite(&input.wire),
			Protocol::Ietf => input.messages.decode_ietf(&input.wire),
		}
	}
}

mod codec_varint {
	use std::fmt;

	use moq_net::fuzz::{decode_varints, encode_varints};
	use swole::bench;

	/// Varints spanning the length classes of both wire forms.
	fn varints() -> Vec<u64> {
		(0..VARINTS as u64)
			.map(|n| match n % 4 {
				0 => n % 60,
				1 => 1_000 + n,
				2 => 1_000_000 + n,
				_ => (1 << 40) + n,
			})
			.collect()
	}

	const VARINTS: usize = 1_024;

	/// A wire form, by the `ietf` flag `encode_varints` takes.
	#[derive(Clone, Copy)]
	struct Form {
		name: &'static str,
		ietf: bool,
	}

	const FORMS: [Form; 2] = [
		Form {
			name: "quic",
			ietf: false,
		},
		Form {
			name: "leading_ones",
			ietf: true,
		},
	];

	impl fmt::Display for Form {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			f.write_str(self.name)
		}
	}

	struct Encoding {
		ietf: bool,
		values: Vec<u64>,
		out: Vec<u8>,
	}

	fn encoding(form: Form) -> Encoding {
		let values = varints();
		let mut out = Vec::new();
		encode_varints(&values, form.ietf, &mut out);
		Encoding {
			ietf: form.ietf,
			values,
			out,
		}
	}

	#[bench(args = FORMS, setup = encoding, items = |_: Form| VARINTS)]
	fn encode(input: &mut Encoding) {
		input.out.clear();
		encode_varints(&input.values, input.ietf, &mut input.out);
	}

	fn wire(form: Form) -> (bool, Vec<u8>) {
		let mut wire = Vec::new();
		encode_varints(&varints(), form.ietf, &mut wire);
		(form.ietf, wire)
	}

	#[bench(args = FORMS, setup = wire, items = |_: Form| VARINTS)]
	fn decode((ietf, wire): &(bool, Vec<u8>)) -> u64 {
		decode_varints(wire, *ietf)
	}
}

fn main() {
	swole::main();
}
