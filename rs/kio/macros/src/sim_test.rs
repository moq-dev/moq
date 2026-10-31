//! The `#[kio::test]` expansion: the test body runs inside `kio::clock::sim::run`.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

pub(crate) fn expand(args: TokenStream, item: TokenStream) -> TokenStream {
	if !args.is_empty() {
		return error("#[kio::test] takes no arguments");
	}

	let mut tokens: Vec<TokenTree> = item.into_iter().collect();
	let is = |token: &TokenTree, name: &str| matches!(token, TokenTree::Ident(ident) if ident.to_string() == name);
	let Some(at) = tokens
		.windows(2)
		.position(|pair| is(&pair[0], "async") && is(&pair[1], "fn"))
	else {
		return error("#[kio::test] expects an `async fn`");
	};
	tokens.remove(at);

	let body = match tokens.pop() {
		Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Brace => body,
		_ => return error("#[kio::test] expects a function body"),
	};

	// `{ ::kio::clock::sim::run(async move <body>) }`
	let mut call: TokenStream = "::kio::clock::sim::run".parse().unwrap();
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
