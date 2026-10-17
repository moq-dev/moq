//! BoltFFI's `#[export]`, `#[data]`, and `#[error]`, expanding to UniFFI's attributes.
//!
//! BoltFFI finds its markers by scanning source text, and only literal attributes count:
//! `#[cfg_attr(feature = "boltffi", data)]` is invisible to it. So the slice spells every
//! marker literally and imports these instead of BoltFFI's in the UniFFI build.

use proc_macro::TokenStream;

fn prefix(attr: &str, item: TokenStream) -> TokenStream {
	let mut out: TokenStream = attr.parse().unwrap();
	out.extend(item);
	out
}

/// `#[uniffi::export]`.
#[proc_macro_attribute]
pub fn export(_args: TokenStream, item: TokenStream) -> TokenStream {
	prefix("#[::uniffi::export]", item)
}

/// `#[derive(uniffi::Record)]`.
#[proc_macro_attribute]
pub fn data(_args: TokenStream, item: TokenStream) -> TokenStream {
	prefix("#[derive(::uniffi::Record)]", item)
}

/// `#[derive(uniffi::Error)]`.
#[proc_macro_attribute]
pub fn error(_args: TokenStream, item: TokenStream) -> TokenStream {
	prefix("#[derive(::uniffi::Error)]", item)
}
