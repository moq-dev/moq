//! Response bytes cross the language boundary; each subscriber runs on a mock transport.

pub(crate) fn fin(version: &str, started: bool, clean: bool, responses: Vec<u8>) -> Vec<u8> {
	let input = serde_json::json!({ "version": version, "started": started, "clean": clean, "responses": responses });
	let output = std::process::Command::new("bun")
		.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test/interop/bare-fin.ts"))
		.arg(input.to_string())
		.output()
		.expect("Bun must be installed for the bare-FIN interop test");
	assert!(
		output.status.success(),
		"JS subscriber failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	serde_json::from_slice(&output.stdout).expect("JS publisher returned response bytes")
}
