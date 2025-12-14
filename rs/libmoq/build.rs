use std::env;
use std::fs;
use std::path::PathBuf;

const LIB_NAME: &str = "moq";

fn main() {
	let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
	let version = env::var("CARGO_PKG_VERSION").unwrap();
	let target_dir = target_dir();

	// Generate C header into target/include/
	let include_dir = target_dir.join("include");
	fs::create_dir_all(&include_dir).expect("Failed to create include directory");
	let header = include_dir.join(format!("{}.h", LIB_NAME));
	cbindgen::Builder::new()
		.with_crate(&crate_dir)
		.with_language(cbindgen::Language::C)
		.generate()
		.expect("Unable to generate bindings")
		.write_to_file(&header);

	// Generate pkg-config file into target/
	let pc_in = PathBuf::from(&crate_dir).join(format!("{}.pc.in", LIB_NAME));
	let pc_out = target_dir.join(format!("{}.pc", LIB_NAME));
	if let Ok(template) = fs::read_to_string(&pc_in) {
		let content = template
			.replace("@PREFIX@", "/usr/local")
			.replace("@VERSION@", &version);
		fs::write(&pc_out, content).expect("Failed to write pkg-config file");
	}
}

fn target_dir() -> PathBuf {
	// Always use the workspace target directory (target/)
	// regardless of whether --target is used or not.
	// CARGO_MANIFEST_DIR is rs/libmoq/, so go up two levels to repo root
	PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
		.parent()
		.expect("Failed to get parent of CARGO_MANIFEST_DIR")
		.parent()
		.expect("Failed to get repo root")
		.join("target")
}
