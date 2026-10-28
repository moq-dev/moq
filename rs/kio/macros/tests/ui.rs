//! Each mistake in `ui/` and the diagnostic it reports.

#[test]
fn ui() {
	// `just rs test` builds through clippy, which prints types with their full paths.
	if !test_env::Env::without(&["RUSTC_WORKSPACE_WRAPPER"]).isolate() {
		return;
	}
	trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
