//! Each mistake in `ui/` and the diagnostic it reports.

#[test]
fn ui() {
	// Compiler wrappers change diagnostic types and paths. Prevent nested Cargo
	// from restoring mbx's saved environment and reintroducing those wrappers.
	if !test_env::Env::without(&["RUSTC_WORKSPACE_WRAPPER", "RUSTC_WRAPPER", "MBX_SESSION_RESTORE"])
		.set("MBX_DISABLE", "1")
		.isolate()
	{
		return;
	}
	trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
