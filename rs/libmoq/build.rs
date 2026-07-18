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

	// Generate the pkg-config file next to the staticlib under the profile dir. Scoping it per profile
	// keeps debug and release builds from clobbering each other's moq.pc.
	let pc_in = PathBuf::from(&crate_dir).join(format!("{}.pc.in", LIB_NAME));
	let pkgconfig_dir = profile_dir.join("lib").join("pkgconfig");
	fs::create_dir_all(&pkgconfig_dir).expect("Failed to create pkgconfig directory");
	let pc_out = pkgconfig_dir.join(format!("{}.pc", LIB_NAME));
	if let Ok(template) = fs::read_to_string(&pc_in) {
		let target = env::var("TARGET").unwrap();
		let libs_private = native_libs(&crate_dir, &target);

		let content = template
			.replace("@VERSION@", &version)
			.replace("@LIBS_PRIVATE@", &libs_private);
		fs::write(&pc_out, content).expect("Failed to write pkg-config file");
	}
}

/// Read the platform's `native-libs/` list and format it for pkg-config `Libs.private`.
///
/// CMakeLists.txt reads the same files, so the two stay in sync by construction.
fn native_libs(crate_dir: &str, target: &str) -> String {
	let platform = if target.contains("apple") {
		"apple"
	} else if target.contains("windows") {
		"windows"
	} else {
		"linux"
	};

	let path = PathBuf::from(crate_dir)
		.join("native-libs")
		.join(format!("{}.txt", platform));
	println!("cargo:rerun-if-changed={}", path.display());

	let list = fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));

	list.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty() && !line.starts_with('#'))
		.map(|entry| match entry.strip_prefix("framework:") {
			Some(framework) => format!("-framework {}", framework),
			None => format!("-l{}", entry),
		})
		.collect::<Vec<_>>()
		.join(" ")
}

fn profile_dir() -> PathBuf {
	// OUT_DIR is set by Cargo based on whether --target is used:
	// - With --target: target/{target}/{profile}/build/{crate}-{hash}/out
	// - Without --target: target/{profile}/build/{crate}-{hash}/out
	// Go up 3 levels to the profile dir. Deriving this from OUT_DIR also handles custom profiles,
	// whose output directory can differ from Cargo's PROFILE value.
	PathBuf::from(env::var("OUT_DIR").unwrap())
		.parent() // build/{crate}-{hash}
		.and_then(|p| p.parent()) // build/
		.and_then(|p| p.parent()) // {profile}/
		.expect("Failed to get profile directory from OUT_DIR")
		.to_path_buf()
}
