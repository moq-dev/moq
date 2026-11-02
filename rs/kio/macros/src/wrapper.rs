//! The `#[kio::wrapper]` expansion: an `async fn` beside the `poll_*` method it waits on.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
	Attribute, FnArg, GenericArgument, GenericParam, Ident, ImplItemFn, Pat, PathArguments, ReturnType, Type,
	ext::IdentExt, punctuated::Punctuated, spanned::Spanned,
};

pub(crate) fn expand(args: TokenStream, method: &ImplItemFn) -> syn::Result<TokenStream> {
	if !args.is_empty() {
		return Err(syn::Error::new_spanned(args, "#[kio::wrapper] takes no arguments"));
	}
	let sig = &method.sig;
	if let Some(token) = &sig.asyncness {
		return Err(syn::Error::new_spanned(
			token,
			"#[kio::wrapper] goes on a poll method, not an `async fn`",
		));
	}
	if let Some(token) = &sig.constness {
		return Err(syn::Error::new_spanned(
			token,
			"#[kio::wrapper] cannot wrap a `const fn`",
		));
	}
	if let Some(token) = &sig.unsafety {
		return Err(syn::Error::new_spanned(
			token,
			"#[kio::wrapper] cannot wrap an `unsafe fn`",
		));
	}
	if let Some(abi) = &sig.abi {
		return Err(syn::Error::new_spanned(
			abi,
			"#[kio::wrapper] cannot wrap an `extern fn`",
		));
	}
	if let Some(variadic) = &sig.variadic {
		return Err(syn::Error::new_spanned(
			variadic,
			"#[kio::wrapper] cannot wrap a variadic fn",
		));
	}

	let poll = &sig.ident;
	let name = poll.to_string();
	let Some(name) = name.strip_prefix("poll_").filter(|name| !name.is_empty()) else {
		return Err(syn::Error::new_spanned(
			poll,
			"#[kio::wrapper] needs a `poll_<name>` method",
		));
	};
	// `poll_match` waits as `r#match`. `poll_self`, `poll__`, and `poll_1` have no
	// such name: those keywords cannot be raw, and a digit cannot start one.
	let name = if syn::parse_str::<Ident>(name).is_ok() {
		Ident::new(name, poll.span())
	} else if syn::parse::Parser::parse_str(Ident::parse_any, name).is_ok()
		&& !matches!(name, "_" | "self" | "Self" | "super" | "crate")
	{
		Ident::new_raw(name, poll.span())
	} else {
		return Err(syn::Error::new_spanned(
			poll,
			format!("#[kio::wrapper] cannot name the wrapper `{name}`, even as a raw identifier"),
		));
	};

	let mut inputs = sig.inputs.iter();
	let receiver = match inputs.next() {
		Some(FnArg::Receiver(receiver)) if receiver.reference.is_some() && receiver.colon_token.is_none() => receiver,
		Some(FnArg::Receiver(receiver)) => {
			return Err(syn::Error::new_spanned(
				receiver,
				"#[kio::wrapper] needs a borrowed receiver, `&self` or `&mut self`",
			));
		}
		_ => {
			return Err(syn::Error::new(
				sig.paren_token.span.join(),
				"#[kio::wrapper] needs a borrowed receiver, `&self` or `&mut self`",
			));
		}
	};

	// Every other argument is passed through to each poll; the waiter is the wrapper's own.
	let waiter = Ident::new("waiter", Span::mixed_site());
	let mut params = Punctuated::<FnArg, syn::Token![,]>::new();
	let mut call = Vec::new();
	let mut found = None;
	for input in inputs {
		let FnArg::Typed(input) = input else {
			return Err(syn::Error::new_spanned(input, "`self` must be the first argument"));
		};
		if let Some(mutable) = waiter_ref(&input.ty) {
			if mutable {
				return Err(syn::Error::new_spanned(
					&input.ty,
					"#[kio::wrapper] needs the waiter as `&Waiter`, not `&mut Waiter`",
				));
			}
			if let Some(first) = found.replace(input) {
				let mut err =
					syn::Error::new_spanned(&input.ty, "#[kio::wrapper] needs exactly one `&Waiter` argument");
				err.combine(syn::Error::new_spanned(&first.ty, "the first `&Waiter` is here"));
				return Err(err);
			}
			call.push(quote! { #waiter });
			continue;
		}
		let Pat::Ident(pat) = input.pat.as_ref() else {
			return Err(syn::Error::new_spanned(
				&input.pat,
				"#[kio::wrapper] needs each argument bound to a plain name",
			));
		};
		if pat.by_ref.is_some() || pat.subpat.is_some() {
			return Err(syn::Error::new_spanned(
				&input.pat,
				"#[kio::wrapper] needs each argument bound to a plain name",
			));
		}
		// `mut` only affects the poll's own binding: the wrapper never rebinds it.
		let mut param = input.clone();
		param.pat = Box::new(Pat::Ident(syn::PatIdent {
			mutability: None,
			..pat.clone()
		}));
		params.push(FnArg::Typed(param));
		let ident = &pat.ident;
		call.push(quote! { #ident });
	}
	if found.is_none() {
		return Err(syn::Error::new(
			sig.paren_token.span.join(),
			"#[kio::wrapper] needs a `&Waiter` argument",
		));
	}

	let output = ready_type(sig)?;

	// Type and const parameters, named so the wrapper never relies on inference to pick them.
	let turbofish = sig
		.generics
		.params
		.iter()
		.filter_map(|param| match param {
			GenericParam::Type(param) => Some(&param.ident),
			GenericParam::Const(param) => Some(&param.ident),
			GenericParam::Lifetime(_) => None,
		})
		.collect::<Vec<_>>();
	let turbofish = match turbofish.is_empty() {
		true => quote! {},
		false => quote! { ::<#(#turbofish),*> },
	};

	let mut wrapper = sig.clone();
	wrapper.ident = name;
	wrapper.asyncness = Some(syn::Token![async](poll.span()));
	wrapper.inputs = std::iter::once(FnArg::Receiver(receiver.clone()))
		.chain(params)
		.collect();
	wrapper.output = match output {
		Type::Tuple(unit) if unit.elems.is_empty() => ReturnType::Default,
		output => syn::parse_quote_spanned!(sig.output.span()=> -> #output),
	};

	let attrs = method.attrs.iter().filter(|attr| copied(attr)).collect::<Vec<_>>();
	let link = format!("Awaits [`{poll}`](Self::{poll}).");
	let link = match attrs.iter().any(|attr| attr.path().is_ident("doc")) {
		true => quote! { #[doc = ""] #[doc = #link] },
		false => quote! { #[doc = #link] },
	};
	// A deprecated wrapper calls its deprecated poll without a warning of its own.
	let allow = match attrs.iter().any(|attr| attr.path().is_ident("deprecated")) {
		true => quote! { #[allow(deprecated, reason = "a deprecated wrapper of a deprecated poll")] },
		false => quote! {},
	};
	let vis = &method.vis;
	// The receiver's own `self`, which a `self` spanned anywhere else would not resolve to.
	let this = &receiver.self_token;

	Ok(quote! {
		#method

		#(#attrs)*
		#link
		#allow
		#vis #wrapper {
			::kio::wait(|#waiter| #this.#poll #turbofish(#(#call),*)).await
		}
	})
}

/// Whether `ty` is a `&mut` if it is a reference to `Waiter` (by any path ending in it).
fn waiter_ref(ty: &Type) -> Option<bool> {
	let Type::Reference(reference) = ty else { return None };
	let Type::Path(path) = reference.elem.as_ref() else {
		return None;
	};
	let waiter = path.qself.is_none()
		&& path
			.path
			.segments
			.last()
			.is_some_and(|segment| segment.ident == "Waiter" && segment.arguments.is_none());
	waiter.then_some(reference.mutability.is_some())
}

/// `T` from `-> Poll<T>`, by any path ending in `Poll`.
fn ready_type(sig: &syn::Signature) -> syn::Result<&Type> {
	const MESSAGE: &str = "#[kio::wrapper] needs a method returning `Poll<T>`";
	let output = &sig.output;
	let error = || syn::Error::new_spanned(output, MESSAGE);
	let ReturnType::Type(_, ty) = output else {
		// No return type to point at: point where it would go.
		return Err(syn::Error::new(sig.paren_token.span.close(), MESSAGE));
	};
	let Type::Path(path) = ty.as_ref() else {
		return Err(error());
	};
	let Some(segment) = path.path.segments.last().filter(|segment| segment.ident == "Poll") else {
		return Err(error());
	};
	let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
		return Err(error());
	};
	match arguments.args.first() {
		Some(GenericArgument::Type(ty)) if arguments.args.len() == 1 => Ok(ty),
		_ => Err(error()),
	}
}

/// What the wrapper inherits: docs, the conditions it compiles under, and deprecation.
fn copied(attr: &Attribute) -> bool {
	["doc", "cfg", "deprecated"]
		.iter()
		.any(|name| attr.path().is_ident(name))
}
