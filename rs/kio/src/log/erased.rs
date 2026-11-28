//! What plain builds see of Verus's proof macros: nothing.
//!
//! Only Verus sets `verus_keep_ghost`, so every other build compiles without `vstd`, and
//! these stand in for the macros it would bring. Each erases its proof, as `vstd`'s do
//! outside Verus, so the code that runs is the same either way.

/// A proof step.
macro_rules! proof {
	($($proof:tt)*) => {};
}

/// Ghost and tracked `let`s.
macro_rules! proof_decl {
	($($proof:tt)*) => {};
}

/// The ghost arguments and results of the next call.
macro_rules! proof_with {
	($($proof:tt)*) => {};
}

/// Open the log's ledger around one atomic access: only the access runs.
macro_rules! open_atomic_invariant {
	($invariant:expr => $inner:ident => $body:block) => {
		$body
	};
}
