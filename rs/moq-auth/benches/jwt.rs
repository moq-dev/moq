//! Signing and verifying one token with each key family, through the public API.
//!
//! Run with `cargo bench -p moq-auth --bench jwt`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use moq_auth::{Algorithm, Claims, Key};

fn bench_jwt(c: &mut Criterion) {
	let claims = Claims::default()
		.with_root("room")
		.with_publish(["alice/**".parse::<moq_auth::Pattern>().unwrap()]);

	let mut group = c.benchmark_group("jwt");
	for algorithm in [Algorithm::HS256, Algorithm::ES256, Algorithm::EdDSA, Algorithm::RS256] {
		let key = Key::generate(algorithm, None).unwrap();
		let token = key.sign(&claims).unwrap();
		group.bench_function(format!("sign_{algorithm}"), |b| {
			b.iter(|| key.sign(black_box(&claims)).unwrap())
		});
		group.bench_function(format!("verify_{algorithm}"), |b| {
			b.iter(|| key.verify(black_box(&token)).unwrap())
		});
	}
	group.finish();
}

criterion_group!(benches, bench_jwt);
criterion_main!(benches);
