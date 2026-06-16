// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Vendored verbatim from cros-libva (BSD-3-Clause); not subject to this
// workspace's strict lints.
#![allow(clippy::all)]

use regex::Regex;
use std::env::VarError;
use std::env::{self};
use std::fs::read_to_string;
use std::path::{Path, PathBuf};

mod bindgen_gen;
use bindgen_gen::vaapi_gen_builder;

/// Environment variable that can be set to point to the directory containing the `va.h`, `va_drm.h` and `va_drmcommon.h`
/// files to use to generate the bindings.
const CROS_LIBVA_H_PATH_ENV: &str = "CROS_LIBVA_H_PATH";
const CROS_LIBVA_LIB_PATH_ENV: &str = "CROS_LIBVA_LIB_PATH";

/// Wrapper file to use as input of bindgen.
const WRAPPER_PATH: &str = "libva-wrapper.h";

// Return VA_MAJOR_VERSION and VA_MINOR_VERSION from va_version.h.
fn get_va_version(va_h_path: &str) -> (u32, u32) {
	let va_version_h_path = Path::new(va_h_path).join("va/va_version.h");
	assert!(
		va_version_h_path.exists(),
		"{} doesn't exist",
		va_version_h_path.display()
	);
	let header_content = read_to_string(va_version_h_path).unwrap();
	let lines = header_content.lines();

	const VERSION_REGEX_STRINGS: [&str; 2] = [
		r"#define VA_MAJOR_VERSION\s*[0-9]+",
		r"#define VA_MINOR_VERSION\s*[0-9]+",
	];
	let mut numbers: [u32; 2] = [0; 2];
	for i in 0..2 {
		let re = Regex::new(VERSION_REGEX_STRINGS[i]).unwrap();
		let match_line = lines.clone().filter(|&s| re.is_match(s)).collect::<Vec<_>>();
		assert_eq!(
			match_line.len(),
			1,
			"unexpected match for {}: {:?}",
			VERSION_REGEX_STRINGS[i],
			match_line
		);
		let number_str = Regex::new(r"[0-9]+").unwrap().find(match_line[0]).unwrap().as_str();
		numbers[i] = number_str.parse::<u32>().unwrap();
	}

	(numbers[0], numbers[1])
}

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

	let (va_h_path, major, minor) = if cfg!(feature = "vendored") {
		let (major, minor) = generate_vendored_version_header(&out_dir);

		// Tell cargo to re-run if submodule files change
		println!("cargo:rerun-if-changed=libva/meson.build");
		println!("cargo:rerun-if-changed=libva/va/va_version.h.in");

		// We need two include paths:
		// 1. The submodule root so `#include <va/va.h>` resolves to `libva/va/va.h`
		// 2. OUT_DIR so `#include <va/va_version.h>` resolves to the generated header
		//
		// Return the submodule path; OUT_DIR is added as an extra clang arg below.
		let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("`CARGO_MANIFEST_DIR` is not set"));
		let va_h_path = manifest_dir.join("libva").into_os_string().into_string().unwrap();
		(va_h_path, major, minor)
	} else {
		let va_h_path = env::var(CROS_LIBVA_H_PATH_ENV)
			.or_else(|e| {
				if let VarError::NotPresent = e {
					// Only need include paths here; linking is handled separately below.
					let mut config = pkg_config::Config::new();
					config.cargo_metadata(false);
					match config.probe("libva") {
						Ok(lib) => Ok(lib.include_paths[0].clone().into_os_string().into_string().unwrap()),
						Err(e) => panic!("libva is not found in system: {}", e),
					}
				} else {
					Err(e)
				}
			})
			.expect("libva header location is unknown");
		let (major, minor) = get_va_version(&va_h_path);
		(va_h_path, major, minor)
	};

	// Check the path exists.
	if !va_h_path.is_empty() {
		assert!(Path::new(&va_h_path).exists(), "{} doesn't exist", va_h_path);
	}

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

	if !cfg!(feature = "dlopen") {
		// Use pkg-config to find library paths for linking (even in vendored mode,
		// where we only vendor headers but still link against the system libva).
		// CROS_LIBVA_LIB_PATH can override the library search path.
		let va_lib_path = env::var(CROS_LIBVA_LIB_PATH_ENV).unwrap_or_default();
		if !va_lib_path.is_empty() {
			assert!(Path::new(&va_lib_path).exists(), "{} doesn't exist", va_lib_path);
			println!("cargo:rustc-link-search=native={}", va_lib_path);
			println!("cargo:rustc-link-arg=-Wl,-rpath={}", va_lib_path);
			println!("cargo:rustc-link-lib=dylib=va");
			println!("cargo:rustc-link-lib=dylib=va-drm");
		} else {
			// Let pkg-config emit the link-search and link-lib directives.
			let mut libva = pkg_config::Config::new();
			libva.cargo_metadata(true);
			libva.probe("libva").expect("libva not found via pkg-config");

			let mut libva_drm = pkg_config::Config::new();
			libva_drm.cargo_metadata(true);
			libva_drm
				.probe("libva-drm")
				.expect("libva-drm not found via pkg-config");
		}
	}

	let mut bindings_builder = vaapi_gen_builder(bindgen::builder()).header(WRAPPER_PATH);
	if !va_h_path.is_empty() {
		bindings_builder = bindings_builder.clang_arg(format!("-I{}", va_h_path));
	}
	if cfg!(feature = "vendored") {
		bindings_builder = bindings_builder.clang_arg(format!("-I{}", out_dir.display()));
	}
	let bindings = bindings_builder.generate().expect("unable to generate bindings");

	bindings
		.write_to_file(out_dir.join("bindings.rs"))
		.expect("Couldn't write bindings!");
}
