//! Each mistake in `ui/` and the diagnostic it reports.

#[test]
fn ui() {
	trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
