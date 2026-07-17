use std::env;
use std::fs;
use std::path::PathBuf;

const LIB_NAME: &str = "moq";

fn main() {
	let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
	let version = env::var("CARGO_PKG_VERSION").unwrap();
	let profile_dir = profile_dir();
	let target_dir = profile_dir.parent().expect("profile dir has no parent");

	// Generate C header into target/include/. The header is profile-independent,
	// so a debug and release build in the same target tree can share it.
	let include_dir = target_dir.join("include");
	fs::create_dir_all(&include_dir).expect("Failed to create include directory");
	let header = include_dir.join(format!("{}.h", LIB_NAME));
	cbindgen::Builder::new()
		.with_crate(&crate_dir)
		.with_language(cbindgen::Language::C)
		.generate()
		.expect("Unable to generate bindings")
		.write_to_file(&header);

	// Generate the pkg-config file next to the staticlib under the profile dir
	// (target/<profile>/lib/pkgconfig/). Scoping it per profile, rather than a
	// shared target/lib/pkgconfig/, keeps a debug and release build from
	// clobbering each other's moq.pc, and lets libdir resolve to the sibling
	// staticlib without a profile placeholder.
	let pc_in = PathBuf::from(&crate_dir).join(format!("{}.pc.in", LIB_NAME));
	let pkgconfig_dir = profile_dir.join("lib").join("pkgconfig");
	fs::create_dir_all(&pkgconfig_dir).expect("Failed to create pkgconfig directory");
	let pc_out = pkgconfig_dir.join(format!("{}.pc", LIB_NAME));
	if let Ok(template) = fs::read_to_string(&pc_in) {
		let target = env::var("TARGET").unwrap();
		let libs_private = if target.contains("apple") {
			"-framework CoreFoundation -framework Security -framework CoreServices"
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

fn profile_dir() -> PathBuf {
	// OUT_DIR is set by Cargo based on whether --target is used:
	// - With --target: target/{target}/{profile}/build/{crate}-{hash}/out
	// - Without --target: target/{profile}/build/{crate}-{hash}/out
	// Go up 3 levels to the profile dir (where the staticlib is written); its
	// parent is target/ or target/{target}/. Deriving from OUT_DIR (rather than
	// the PROFILE env var) stays correct for custom profiles, whose output dir
	// name is the profile name even though PROFILE reports "debug"/"release".
	PathBuf::from(env::var("OUT_DIR").unwrap())
		.parent() // build/{crate}-{hash}
		.and_then(|p| p.parent()) // build/
		.and_then(|p| p.parent()) // {profile}/
		.expect("Failed to get profile directory from OUT_DIR")
		.to_path_buf()
}
