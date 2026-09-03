//! Write the generated fuzzer corpus to disk: `fuzz-seeds <dir>` fills
//! `<dir>/<target>/` with one file per seed.
//!
//! `just rs fuzz` runs this before each session, so the corpus always matches the
//! dispatch in `moq_net::fuzz` rather than whatever was generated last.

fn main() {
	let root = std::env::args().nth(1).expect("usage: fuzz-seeds <dir>");
	let root = std::path::Path::new(&root);

	for (target, _) in moq_net::fuzz::TARGETS {
		let dir = root.join(target);
		std::fs::create_dir_all(&dir).expect("could not create the seed directory");
	}

	let seeds = moq_net::fuzz::seeds();
	for (index, seed) in seeds.iter().enumerate() {
		let path = root.join(seed.target).join(format!("{index}.bin"));
		std::fs::write(&path, &seed.data).expect("could not write a seed");
	}

	println!("wrote {} seeds to {}", seeds.len(), root.display());
}
