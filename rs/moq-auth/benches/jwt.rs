//! Signing and verifying one token with each key family, through the public API.
//!
//! Run with `cargo bench --manifest-path rs/moq-auth/Cargo.toml --bench jwt`.

use moq_auth::{Algorithm, Claims};

const ALGORITHMS: [Algorithm; 4] = [Algorithm::HS256, Algorithm::ES256, Algorithm::EdDSA, Algorithm::RS256];

fn claims() -> Claims {
	Claims::default()
		.with_root("room")
		.with_publish(["alice".parse::<moq_auth::Prefix>().unwrap()])
}

mod jwt {
	use moq_auth::{Algorithm, Claims, Key};
	use swole::bench;

	use super::{ALGORITHMS, claims};

	fn signer(algorithm: Algorithm) -> (Key, Claims) {
		(Key::generate(algorithm, None).unwrap(), claims())
	}

	#[bench(args = ALGORITHMS, setup = signer)]
	fn sign((key, claims): &(Key, Claims)) -> String {
		key.sign(claims).unwrap()
	}

	fn token(algorithm: Algorithm) -> (Key, String) {
		let key = Key::generate(algorithm, None).unwrap();
		let token = key.sign(&claims()).unwrap();
		(key, token)
	}

	#[bench(args = ALGORITHMS, setup = token)]
	fn verify((key, token): &(Key, String)) -> Claims {
		key.verify(token).unwrap()
	}
}

fn main() {
	swole::main();
}
