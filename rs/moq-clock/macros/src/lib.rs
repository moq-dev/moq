//! The `#[moq_clock::test]` attribute: an `async fn` test run on the simulator.
//!
//! Use it through `moq-clock`'s `sim` feature, which re-exports it.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

/// Run an `async fn` test to completion on `moq_clock::sim::run`.
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
	if !args.is_empty() {
		return error("#[moq_clock::test] takes no arguments");
	}

	let mut tokens: Vec<TokenTree> = item.into_iter().collect();
	let is = |token: &TokenTree, name: &str| matches!(token, TokenTree::Ident(ident) if ident.to_string() == name);
	let Some(at) = tokens
		.windows(2)
		.position(|pair| is(&pair[0], "async") && is(&pair[1], "fn"))
	else {
		return error("#[moq_clock::test] expects an `async fn`");
	};
	tokens.remove(at);

	let body = match tokens.pop() {
		Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Brace => body,
		_ => return error("#[moq_clock::test] expects a function body"),
	};

	// `{ ::moq_clock::sim::run(async move <body>) }`
	let mut call: TokenStream = "::moq_clock::sim::run".parse().unwrap();
	let block: TokenStream = [
		TokenTree::Ident(Ident::new("async", Span::call_site())),
		TokenTree::Ident(Ident::new("move", Span::call_site())),
		TokenTree::Group(body),
	]
	.into_iter()
	.collect();
	call.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, block))]);

	let mut out: TokenStream = "#[::core::prelude::v1::test]".parse().unwrap();
	out.extend(tokens);
	out.extend([TokenTree::Group(Group::new(Delimiter::Brace, call))]);
	out
}

fn error(message: &str) -> TokenStream {
	format!("::core::compile_error!({message:?});").parse().unwrap()
}
