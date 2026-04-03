use std::process::Command;

fn main() {
	println!("cargo:rerun-if-changed=../../.git/HEAD");
	println!("cargo:rerun-if-changed=../../.git/refs/heads");
	println!("cargo:rerun-if-changed=../../.git/refs/tags");
	println!("cargo:rerun-if-changed=../../.git/packed-refs");

	let prefix = "moq-clock-v";
	let version = git_version(prefix).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
	println!("cargo:rustc-env=VERSION={version}");
}

fn git_version(prefix: &str) -> Option<String> {
	let output = Command::new("git")
		.args(["describe", "--tags", "--match", &format!("{prefix}*")])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let desc = String::from_utf8(output.stdout).ok()?.trim().to_string();
	let version = desc.strip_prefix(prefix)?;
	// "0.10.11-3-gabcdef" → "0.10.11-abcdef" (drop count, strip 'g')
	if let Some((base, hash)) = version.rsplit_once("-g") {
		let base = base.rsplit_once('-').map(|(b, _)| b).unwrap_or(base);
		Some(format!("{base}-{hash}"))
	} else {
		Some(version.to_string())
	}
}
