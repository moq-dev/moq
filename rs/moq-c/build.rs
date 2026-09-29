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
	// The header lands in OUT_DIR: cargo forbids writing anywhere else, and a
	// build cache that replays this script (mbx) restores OUT_DIR and nothing
	// more. Consumers ask cargo for the path (`build-script-executed` in
	// --message-format=json). moq-c.pc is not written here: its libdir has to name
	// the directory holding libmoq.a, which only exists once packaged (see
	// nix/overlay.nix), since cargo puts the staticlib outside OUT_DIR.
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
		// C++ has to see these declarations with C linkage. Emitting the `extern "C"`
		// block here saves every C++ consumer from wrapping the include by hand, which
		// also wraps the system headers moq.h pulls in.
		cpp_compat: true,
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
}
