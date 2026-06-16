// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Vendored verbatim from cros-libva (BSD-3-Clause); not subject to this
// workspace's strict lints.
#![allow(clippy::all)]

use regex::Regex;
use std::env::{self};
use std::fs::read_to_string;
use std::path::{Path, PathBuf};

mod bindgen_gen;
use bindgen_gen::vaapi_gen_builder;

/// Wrapper file to use as input of bindgen.
const WRAPPER_PATH: &str = "libva-wrapper.h";

/// When using vendored headers, generate `va_version.h` from the submodule's
/// `va_version.h.in` template by extracting version numbers from `meson.build`.
fn generate_vendored_version_header(out_dir: &Path) -> (u32, u32) {
	let meson_build =
		read_to_string("libva/meson.build").expect("failed to read libva/meson.build — is the submodule initialized?");

	// Extract va_api_{major,minor,micro}_version from meson.build
	let extract = |var_name: &str| -> String {
		let re = Regex::new(&format!(r"{}\s*=\s*(\d+)", var_name)).unwrap();
		re.captures(&meson_build)
			.unwrap_or_else(|| panic!("{} not found in libva/meson.build", var_name))[1]
			.to_string()
	};

	let major = extract("va_api_major_version");
	let minor = extract("va_api_minor_version");
	let micro = extract("va_api_micro_version");
	let version = format!("{}.{}.{}", major, minor, micro);

	let template = read_to_string("libva/va/va_version.h.in").expect("failed to read libva/va/va_version.h.in");

	let generated = template
		.replace("@VA_API_MAJOR_VERSION@", &major)
		.replace("@VA_API_MINOR_VERSION@", &minor)
		.replace("@VA_API_MICRO_VERSION@", &micro)
		.replace("@VA_API_VERSION@", &version);

	let va_dir = out_dir.join("va");
	std::fs::create_dir_all(&va_dir).expect("failed to create va dir in OUT_DIR");
	std::fs::write(va_dir.join("va_version.h"), generated).expect("failed to write va_version.h");

	// libva keeps va_drm.h under va/drm/ in its source tree, but installs it (and
	// the wrapper includes it) as <va/va_drm.h>. Stage it next to the generated
	// va_version.h so the OUT_DIR include path resolves <va/va_drm.h>.
	std::fs::copy("libva/va/drm/va_drm.h", va_dir.join("va_drm.h")).expect("failed to stage va/drm/va_drm.h");

	(
		major.parse().expect("invalid major version"),
		minor.parse().expect("invalid minor version"),
	)
}

fn main() {
	// Do not require dependencies when generating docs.
	if std::env::var("CARGO_DOC").is_ok() || std::env::var("DOCS_RS").is_ok() {
		return;
	}

	let out_dir = PathBuf::from(env::var("OUT_DIR").expect("`OUT_DIR` is not set"));

	// Always build against the vendored libva headers (the rs/moq-vaapi/libva
	// submodule, pinned to a known VA-API version) and dlopen libva at runtime
	// rather than link it. There is no system-libva path, so the build needs only
	// libclang for bindgen and works on any OS.
	let (major, minor) = generate_vendored_version_header(&out_dir);
	println!("cargo:rerun-if-changed=libva/meson.build");
	println!("cargo:rerun-if-changed=libva/va/va_version.h.in");

	// Two include paths: the submodule root so `#include <va/va.h>` resolves to
	// `libva/va/va.h`, and OUT_DIR for the generated `<va/va_version.h>` and the
	// staged `<va/va_drm.h>`.
	let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("`CARGO_MANIFEST_DIR` is not set"));
	let va_h_path = manifest_dir.join("libva");
	assert!(
		va_h_path.join("meson.build").exists(),
		"{} is empty — run `git submodule update --init rs/moq-vaapi/libva`",
		va_h_path.display()
	);

	println!("libva {}.{} is used to generate bindings", major, minor);
	let va_check_version = |desired_major: u32, desired_minor: u32| {
		major > desired_major || (major == desired_major && minor >= desired_minor)
	};

	// Declare the custom cfg flags to avoid warnings
	println!("cargo::rustc-check-cfg=cfg(libva_1_21_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_20_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_19_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_16_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_15_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_14_or_higher)");
	println!("cargo::rustc-check-cfg=cfg(libva_1_10_or_higher)");

	// Set the cfg flags based on version
	if va_check_version(1, 21) {
		println!("cargo::rustc-cfg=libva_1_21_or_higher");
	}
	if va_check_version(1, 20) {
		println!("cargo::rustc-cfg=libva_1_20_or_higher")
	}
	if va_check_version(1, 19) {
		println!("cargo::rustc-cfg=libva_1_19_or_higher")
	}
	if va_check_version(1, 16) {
		println!("cargo::rustc-cfg=libva_1_16_or_higher")
	}
	if va_check_version(1, 15) {
		println!("cargo::rustc-cfg=libva_1_15_or_higher");
	}
	if va_check_version(1, 14) {
		println!("cargo::rustc-cfg=libva_1_14_or_higher");
	}
	if va_check_version(1, 10) {
		println!("cargo::rustc-cfg=libva_1_10_or_higher");
	}

	let bindings = vaapi_gen_builder(bindgen::builder())
		.header(WRAPPER_PATH)
		.clang_arg(format!("-I{}", va_h_path.display()))
		.clang_arg(format!("-I{}", out_dir.display()))
		.generate()
		.expect("unable to generate bindings");

	bindings
		.write_to_file(out_dir.join("bindings.rs"))
		.expect("Couldn't write bindings!");
}
