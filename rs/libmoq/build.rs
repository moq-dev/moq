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
		let target = env::var("TARGET").unwrap();
		let libs_private = if target.contains("apple") {
			"-framework CoreFoundation -framework Security"
		} else if target.contains("windows") {
			"-lws2_32 -lbcrypt -luserenv -lntdll"
		} else {
			"-ldl -lm -lpthread"
		};

		let content = template
			.replace("@VERSION@", &version)
			.replace("@LIBS_PRIVATE@", libs_private);
		fs::write(&pc_out, content).expect("Failed to write pkg-config file");
	}
}

fn target_dir() -> PathBuf {
	// OUT_DIR is always set by Cargo to something like:
	// target/{debug|release}/build/{crate}-{hash}/out
	// Go up 4 levels to get to target/
	PathBuf::from(env::var("OUT_DIR").unwrap())
		.parent() // build/{crate}-{hash}
		.and_then(|p| p.parent()) // build/
		.and_then(|p| p.parent()) // {debug|release}/
		.and_then(|p| p.parent()) // target/
		.expect("Failed to get target directory from OUT_DIR")
		.to_path_buf()
}
