use std::env;
use std::fs;
use std::path::PathBuf;

const LIB_NAME: &str = "moq";

/// Enums the header must declare even though no signature mentions them.
const ENUMS: &[&str] = &[
	"moq_container_kind",
	"moq_audio_format",
	"moq_audio_sample_format",
	"moq_video_format",
	"moq_container_format",
	"moq_video_pixel_format",
	"moq_video_codec",
	"moq_video_encoder_kind",
	"moq_error_scope",
	"moq_protocol_kind",
	"moq_demand",
];

fn main() {
	let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
	let version = env::var("CARGO_PKG_VERSION").unwrap();
	// Everything lands in OUT_DIR, laid out like the install prefix minus the
	// staticlib. Cargo forbids writing anywhere else, and a build cache that
	// replays this script (mbx) restores OUT_DIR and nothing more. Consumers ask
	// cargo for the path (`build-script-executed` in --message-format=json).
	let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

	// The `rerun-if-changed` below opts out of cargo's default "rerun when any
	// file in the package changes", so the sources cbindgen reads have to be
	// named explicitly or an edited signature leaves a stale header behind.
	println!("cargo:rerun-if-changed=src");

	let include_dir = out_dir.join("include");
	fs::create_dir_all(&include_dir).expect("Failed to create include directory");
	let header = include_dir.join(format!("{}.h", LIB_NAME));
	let config = cbindgen::Config {
		header: Some("/* Error codes -1, -11, -12, and -39 are retired and reserved. */".into()),
		// cbindgen.toml is never loaded (see its header comment), so the generated
		// header has no include guard unless we ask for one here. Without it a
		// project reaching moq.h down two include paths gets redefinition errors.
		pragma_once: true,
		export: cbindgen::ExportConfig {
			// These enums cross the ABI as plain `uint32_t`, so that an unknown
			// discriminant from C is an error rather than UB. That leaves no signature
			// referencing them, and cbindgen emits only what a signature reaches, so
			// name them here: without this a C caller has to hardcode the integers.
			include: ENUMS.iter().map(|name| name.to_string()).collect(),
			..Default::default()
		},
		..Default::default()
	};
	cbindgen::Builder::new()
		.with_crate(&crate_dir)
		.with_config(config)
		.with_language(cbindgen::Language::C)
		.generate()
		.expect("Unable to generate bindings")
		.write_to_file(&header);

	let pc_in = PathBuf::from(&crate_dir).join(format!("{}.pc.in", LIB_NAME));
	let pkgconfig_dir = out_dir.join("lib").join("pkgconfig");
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
