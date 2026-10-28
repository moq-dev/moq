//! The `#[kio::machine]` attribute: a `poll_*` state machine from straight-line code.
//!
//! Use it through `kio`'s `macros` feature, which re-exports it.

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::{ToTokens, quote};
use syn::{
	Expr, Ident, ImplItem, ItemImpl, Macro, Pat, Stmt, Token, Type,
	parse::{Parse, ParseStream},
	parse_macro_input,
	visit_mut::{self, VisitMut},
};

/// Write one `poll_*` method of an `impl` as straight-line code with explicit waits.
///
/// `#[kio::machine(self.state: Name)]` on the `impl` generates `enum Name`, one
/// variant per wait point holding the locals that live across it, and rewrites the
/// method that uses `wait!` to resume from `self.state`. The struct declares
/// `state: Name` and starts it at `Name::Start`.
///
/// - `let x: T = wait!(Step => poll)?;` evaluates `poll` (a `Poll<_>` expression)
///   on every call while it is pending, then continues. `Step` names the variant.
///   Inside `poll`, earlier locals are `&mut` borrows of the stored state, and a
///   `return` or `ready!` leaves the state where it is.
/// - `#[every_poll]` statements lead the body and run on every call, before resuming.
/// - Any other statement runs once. It must not return `Poll::Pending` or use `ready!`.
/// - A local used after a wait must be bound as `let name: Type`, since its type
///   becomes a field. It is dropped at the first wait after its last use.
/// - The body returns `Poll::Ready(_)` like any `poll_*`, and the machine is `Done`.
#[proc_macro_attribute]
pub fn machine(args: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
	let args = parse_macro_input!(args as Args);
	let item = parse_macro_input!(item as ItemImpl);
	expand(args, item).unwrap_or_else(syn::Error::into_compile_error).into()
}

struct Args {
	field: Ident,
	state: Ident,
}

impl Parse for Args {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		input.parse::<Token![self]>()?;
		input.parse::<Token![.]>()?;
		let field = input.parse()?;
		input.parse::<Token![:]>()?;
		let state = input.parse()?;
		Ok(Self { field, state })
	}
}

fn expand(args: Args, mut item: ItemImpl) -> syn::Result<TokenStream> {
	if !item.generics.params.is_empty() {
		return Err(syn::Error::new_spanned(
			&item.generics,
			"#[kio::machine] does not support generic impls yet",
		));
	}

	let mut found = None;
	for impl_item in &mut item.items {
		let ImplItem::Fn(method) = impl_item else { continue };
		if !Waits::any(&method.block) {
			continue;
		}
		if found.is_some() {
			return Err(syn::Error::new_spanned(
				&method.sig,
				"#[kio::machine] lowers one method per impl",
			));
		}
		let (body, variants) = lower(&args, &method.block.stmts)?;
		method.block.stmts = vec![Stmt::Expr(syn::parse2(body)?, None)];
		found = Some((method.sig.ident.clone(), variants));
	}
	let Some((method, variants)) = found else {
		return Err(syn::Error::new(
			Span::call_site(),
			"no method in this impl uses `wait!`",
		));
	};

	let state = &args.state;
	let self_ty = &item.self_ty;
	let doc = format!(
		"Where `{}::{method}` is waiting.",
		self_ty.to_token_stream().to_string().replace(' ', "")
	);
	let defs = variants.iter().map(|variant| {
		let name = &variant.name;
		let fields = variant.stored.iter().map(|local| {
			let ident = &local.ident;
			let ty = &local.ty;
			quote! { #ident: #ty }
		});
		quote! { #name { #(#fields),* } }
	});
	let names = variants.iter().map(|variant| {
		let name = &variant.name;
		let label = name.to_string();
		quote! { #state::#name { .. } => #label }
	});

	Ok(quote! {
		#item

		#[doc = #doc]
		#[allow(clippy::large_enum_variant, reason = "a machine's enum is its storage")]
		enum #state {
			/// Not polled yet.
			Start,
			#(#defs,)*
			/// Returned `Poll::Ready`.
			Done,
		}

		impl #state {
			/// The wait point's name, for logs and drop handlers.
			#[allow(dead_code, reason = "inspection only")]
			fn name(&self) -> &'static str {
				match self {
					#state::Start => "Start",
					#(#names,)*
					#state::Done => "Done",
				}
			}
		}
	})
}

struct Variant {
	name: Ident,
	stored: Vec<Stored>,
}

struct Stored {
	ident: Ident,
	ty: Type,
	mutable: bool,
	/// The wait's poll reads it, by `&mut`.
	polled: bool,
	/// A later statement reads it, by value.
	after: bool,
}

struct Wait {
	name: Ident,
	poll: Expr,
}

impl Parse for Wait {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let name = input.parse()?;
		input.parse::<Token![=>]>()?;
		let poll = input.parse()?;
		Ok(Self { name, poll })
	}
}

/// A top-level `let` binding, in scope until shadowed.
struct Local {
	ident: Ident,
	ty: Option<Type>,
	mutable: bool,
}

fn lower(args: &Args, stmts: &[Stmt]) -> syn::Result<(TokenStream, Vec<Variant>)> {
	let ready = Ident::new("ready", Span::mixed_site());

	// `#[every_poll]` statements lead the body.
	let mut every = Vec::new();
	let mut rest = stmts.iter().peekable();
	while let Some(stmt) = rest.peek() {
		let Some(stmt) = strip_every_poll(stmt)? else { break };
		if Waits::any_stmt(&stmt) {
			return Err(syn::Error::new_spanned(stmt, "`wait!` cannot run every poll"));
		}
		every.push(stmt);
		rest.next();
	}

	// Split the rest at each wait: `segments[k + 1]` starts with the statement
	// that `waits[k]` resumes into.
	let mut segments: Vec<Vec<Stmt>> = vec![Vec::new()];
	let mut waits: Vec<Wait> = Vec::new();
	for stmt in rest {
		if let Some(at) = strip_every_poll(stmt)? {
			return Err(syn::Error::new_spanned(at, "`#[every_poll]` statements lead the body"));
		}
		let mut stmt = stmt.clone();
		let wait = head_wait(&mut stmt, &ready)?;
		if let Some(span) = Waits::first(&stmt) {
			return Err(syn::Error::new(
				span,
				"`wait!` must start a top-level statement: `let x: T = wait!(Step => poll)?;`",
			));
		}
		if let Some(wait) = wait {
			waits.push(wait);
			segments.push(Vec::new());
		}
		segments.last_mut().unwrap().push(stmt);
	}

	for (k, segment) in segments.iter().enumerate() {
		for stmt in segment {
			check_segment(stmt)?;
		}
		if k < waits.len() {
			check_loops(&waits[k].poll)?;
		}
	}

	// The last statement is the value.
	let last = segments.last_mut().unwrap();
	match last.pop() {
		Some(Stmt::Expr(value, None)) => last.push(syn::parse2(quote! { return #value; })?),
		Some(stmt @ Stmt::Expr(Expr::Return(_), _)) => last.push(stmt),
		other => {
			let span = other.map_or_else(Span::call_site, |stmt| syn::spanned::Spanned::span(&stmt));
			return Err(syn::Error::new(
				span,
				"a #[kio::machine] body ends in its `Poll::Ready` value",
			));
		}
	}

	// Tokenize each statement once: every token is a call into the compiler.
	let reads: Vec<Vec<Reads>> = segments
		.iter()
		.map(|segment| segment.iter().map(Reads::new).collect())
		.collect();
	let polls: Vec<HashSet<String>> = waits.iter().map(|wait| uses(wait.poll.to_token_stream())).collect();

	// Each wait stores the in-scope locals its poll or a later statement reads.
	// Each local remembers its declaring statement, by segment and index.
	let mut scope: Vec<(Local, (usize, usize))> = Vec::new();
	// A stored `let mut` may be mutated only after a wait, out of rustc's sight.
	let mut quiet = HashSet::new();
	let mut variants = Vec::new();
	for (k, wait) in waits.iter().enumerate() {
		for (i, stmt) in segments[k].iter().enumerate() {
			if let Stmt::Local(local) = stmt {
				for binding in bindings(&local.pat) {
					scope.retain(|(old, _)| old.ident != binding.ident);
					scope.push((binding, (k, i)));
				}
			}
		}

		let mut stored = Vec::new();
		for (local, decl) in &scope {
			let name = local.ident.to_string();
			let polled = polls[k].contains(&name);
			let after = used_later(&name, &reads[k + 1..], &polls[k + 1..]);
			if !polled && !after {
				continue;
			}
			let Some(ty) = local.ty.clone() else {
				return Err(syn::Error::new_spanned(
					&local.ident,
					format!(
						"`{}` lives across `wait!({} => ..)`, so it is stored in `{}::{}`: give it a type, `let {}: Type`",
						local.ident, wait.name, args.state, wait.name, local.ident,
					),
				));
			};
			if local.mutable {
				quiet.insert(*decl);
			}
			stored.push(Stored {
				ident: local.ident.clone(),
				ty,
				mutable: local.mutable,
				polled,
				after,
			});
		}
		variants.push(Variant {
			name: wait.name.clone(),
			stored,
		});
	}

	for (k, i) in quiet {
		if let Stmt::Local(local) = &mut segments[k][i] {
			local.attrs.push(syn::parse_quote!(#[allow(unused_mut)]));
		}
	}

	let Args { field, state } = args;
	let store = |variant: &Variant| {
		let name = &variant.name;
		let idents = variant.stored.iter().map(|local| &local.ident);
		quote! { self.#field = #state::#name { #(#idents),* }; }
	};

	let first = &segments[0];
	let first_store = store(&variants[0]);
	let mut arms = vec![quote! {
		#state::Start => {
			self.#field = #state::Done;
			#(#first)*
			#first_store
		}
	}];

	for (k, (wait, variant)) in waits.iter().zip(&variants).enumerate() {
		let name = &variant.name;
		let poll = &wait.poll;
		let borrowed = variant
			.stored
			.iter()
			.filter(|local| local.polled)
			.map(|local| &local.ident);
		let owned: Vec<_> = variant
			.stored
			.iter()
			.filter(|local| local.after)
			.map(|local| {
				let ident = &local.ident;
				match local.mutable {
					true => quote! { #ident: mut #ident },
					false => quote! { #ident },
				}
			})
			.collect();
		let take = if owned.is_empty() {
			quote! { self.#field = #state::Done; }
		} else {
			quote! {
				#[allow(unused_mut)]
				let #state::#name { #(#owned,)* .. } = ::core::mem::replace(&mut self.#field, #state::Done) else {
					::core::unreachable!()
				};
			}
		};
		let segment = &segments[k + 1];
		let next = variants.get(k + 1).map(store);
		arms.push(quote! {
			#state::#name { .. } => {
				let #ready = {
					#[allow(unused_variables)]
					let #state::#name { #(#borrowed,)* .. } = &mut self.#field else {
						::core::unreachable!()
					};
					match #poll {
						::core::task::Poll::Ready(#ready) => #ready,
						::core::task::Poll::Pending => return ::core::task::Poll::Pending,
					}
				};
				#take
				#(#segment)*
				#next
			}
		});
	}

	let body = quote! {{
		#(#every)*
		loop {
			match self.#field {
				#(#arms)*
				#state::Done => ::core::panic!("polled after returning `Poll::Ready`"),
			}
		}
	}};
	Ok((body, variants))
}

/// The statement without its leading `#[every_poll]`, if it has one.
fn strip_every_poll(stmt: &Stmt) -> syn::Result<Option<Stmt>> {
	let mut tokens = stmt.to_token_stream().into_iter();
	let (Some(TokenTree::Punct(pound)), Some(TokenTree::Group(attr))) = (tokens.next(), tokens.next()) else {
		return Ok(None);
	};
	if pound.as_char() != '#' || attr.stream().to_string() != "every_poll" {
		return Ok(None);
	}
	let rest: TokenStream = tokens.collect();
	Ok(Some(syn::parse2(rest)?))
}

/// Takes the `wait!` this statement evaluates first, leaving `ready` in its place.
fn head_wait(stmt: &mut Stmt, ready: &Ident) -> syn::Result<Option<Wait>> {
	fn head(expr: &mut Expr, ready: &Ident) -> syn::Result<Option<Wait>> {
		match expr {
			Expr::Macro(mac) if is_wait(&mac.mac) => {
				let wait = mac.mac.parse_body()?;
				// Point type errors on the value at the `wait!`, not the attribute.
				let ready = Ident::new("ready", ready.span().located_at(syn::spanned::Spanned::span(&mac.mac)));
				*expr = syn::parse_quote!(#ready);
				Ok(Some(wait))
			}
			Expr::Try(inner) => head(&mut inner.expr, ready),
			Expr::MethodCall(inner) => head(&mut inner.receiver, ready),
			Expr::Field(inner) => head(&mut inner.base, ready),
			Expr::Binary(inner) => head(&mut inner.left, ready),
			Expr::Cast(inner) => head(&mut inner.expr, ready),
			Expr::Paren(inner) => head(&mut inner.expr, ready),
			Expr::Unary(inner) => head(&mut inner.expr, ready),
			Expr::Reference(inner) => head(&mut inner.expr, ready),
			Expr::Index(inner) => head(&mut inner.expr, ready),
			// A path call evaluates nothing before its first argument.
			Expr::Call(inner) if matches!(*inner.func, Expr::Path(_)) => match inner.args.first_mut() {
				Some(first) => head(first, ready),
				None => Ok(None),
			},
			_ => Ok(None),
		}
	}

	match stmt {
		Stmt::Local(local) => match &mut local.init {
			Some(init) => head(&mut init.expr, ready),
			None => Ok(None),
		},
		Stmt::Expr(inner, _) => head(inner, ready),
		// A bare `wait!(..);` must be ready with `()`.
		Stmt::Macro(mac) if is_wait(&mac.mac) => {
			let wait = mac.mac.parse_body()?;
			let ready = Ident::new("ready", ready.span().located_at(syn::spanned::Spanned::span(&mac.mac)));
			*stmt = syn::parse_quote!(let () = #ready;);
			Ok(Some(wait))
		}
		_ => Ok(None),
	}
}

fn is_wait(mac: &Macro) -> bool {
	mac.path.is_ident("wait")
}

/// Finds `wait!` anywhere, including inside closures.
#[derive(Default)]
struct Waits(Option<Span>);

impl Waits {
	fn any(block: &syn::Block) -> bool {
		let mut waits = Self::default();
		waits.visit_block_mut(&mut block.clone());
		waits.0.is_some()
	}

	fn any_stmt(stmt: &Stmt) -> bool {
		Self::first(stmt).is_some()
	}

	fn first(stmt: &Stmt) -> Option<Span> {
		let mut waits = Self::default();
		waits.visit_stmt_mut(&mut stmt.clone());
		waits.0
	}
}

impl VisitMut for Waits {
	fn visit_macro_mut(&mut self, mac: &mut Macro) {
		if is_wait(mac) && self.0.is_none() {
			self.0 = Some(syn::spanned::Spanned::span(mac));
		}
	}
}

/// A run-once statement yields only through `wait!`.
fn check_segment(stmt: &Stmt) -> syn::Result<()> {
	fn hidden(tokens: TokenStream) -> Option<Span> {
		let mut tokens = tokens.into_iter().peekable();
		while let Some(token) = tokens.next() {
			match token {
				TokenTree::Ident(ident) if ident == "Pending" => return Some(ident.span()),
				TokenTree::Ident(ident)
					if (ident == "ready" || ident == "pending")
						&& matches!(tokens.peek(), Some(TokenTree::Punct(bang)) if bang.as_char() == '!') =>
				{
					return Some(ident.span());
				}
				TokenTree::Group(group) => {
					if let Some(span) = hidden(group.stream()) {
						return Some(span);
					}
				}
				_ => {}
			}
		}
		None
	}

	if let Some(span) = hidden(stmt.to_token_stream()) {
		return Err(syn::Error::new(
			span,
			"this statement runs once, so it cannot return `Poll::Pending`: wait with `let x: T = wait!(Step => poll)?;`",
		));
	}
	let mut stmt = stmt.clone();
	let mut loops = Loops::default();
	loops.visit_stmt_mut(&mut stmt);
	loops.0.map_or(Ok(()), Err)
}

fn check_loops(poll: &Expr) -> syn::Result<()> {
	let mut loops = Loops::default();
	loops.visit_expr_mut(&mut poll.clone());
	loops.0.map_or(Ok(()), Err)
}

/// An unlabeled `break` or `continue` outside the user's own loops would exit the
/// machine's resume loop instead.
#[derive(Default)]
struct Loops(Option<syn::Error>, usize);

impl VisitMut for Loops {
	fn visit_expr_mut(&mut self, expr: &mut Expr) {
		match expr {
			Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_) => {
				self.1 += 1;
				visit_mut::visit_expr_mut(self, expr);
				self.1 -= 1;
			}
			Expr::Closure(_) | Expr::Async(_) => {}
			Expr::Break(syn::ExprBreak { label: None, .. }) | Expr::Continue(syn::ExprContinue { label: None, .. })
				if self.1 == 0 && self.0.is_none() =>
			{
				self.0 = Some(syn::Error::new_spanned(
					expr,
					"`break` and `continue` need a loop of their own",
				));
			}
			_ => visit_mut::visit_expr_mut(self, expr),
		}
	}

	fn visit_item_mut(&mut self, _: &mut syn::Item) {}
}

/// The names a top-level `let` binds. Only `let name: Type` carries a type.
fn bindings(pat: &Pat) -> Vec<Local> {
	if let Pat::Type(typed) = pat
		&& let Pat::Ident(ident) = &*typed.pat
		&& ident.by_ref.is_none()
		&& ident.subpat.is_none()
	{
		return vec![Local {
			ident: ident.ident.clone(),
			ty: Some((*typed.ty).clone()),
			mutable: ident.mutability.is_some(),
		}];
	}

	#[derive(Default)]
	struct Idents(Vec<Local>);
	impl VisitMut for Idents {
		fn visit_pat_ident_mut(&mut self, ident: &mut syn::PatIdent) {
			self.0.push(Local {
				ident: ident.ident.clone(),
				ty: None,
				mutable: ident.mutability.is_some(),
			});
			visit_mut::visit_pat_ident_mut(self, ident);
		}
	}
	let mut idents = Idents::default();
	idents.visit_pat_mut(&mut pat.clone());
	idents.0
}

/// Every identifier that could name a local: not a field, method, or path tail.
fn uses(tokens: TokenStream) -> HashSet<String> {
	fn walk(tokens: TokenStream, into: &mut HashSet<String>) {
		let punct = |token: &Option<TokenTree>, c: char| matches!(token, Some(TokenTree::Punct(p)) if p.as_char() == c);
		let (mut prev, mut prev2) = (None, None);
		for token in tokens {
			let tail = punct(&prev, '.') || (punct(&prev, ':') && punct(&prev2, ':'));
			match &token {
				TokenTree::Ident(ident) if !tail => {
					into.insert(ident.to_string());
				}
				TokenTree::Group(group) => walk(group.stream(), into),
				_ => {}
			}
			prev2 = prev.replace(token);
		}
	}
	let mut into = HashSet::new();
	walk(tokens, &mut into);
	into
}

/// What a statement reads, and what it binds if it is a `let`.
struct Reads {
	uses: HashSet<String>,
	binds: Vec<String>,
}

impl Reads {
	fn new(stmt: &Stmt) -> Self {
		let Stmt::Local(local) = stmt else {
			return Self {
				uses: uses(stmt.to_token_stream()),
				binds: Vec::new(),
			};
		};
		// The initializer only: the pattern names what it binds, not what it reads.
		let uses = local.init.as_ref().map_or_else(HashSet::new, |init| {
			let mut tokens = init.expr.to_token_stream();
			if let Some((_, diverge)) = &init.diverge {
				tokens.extend(diverge.to_token_stream());
			}
			self::uses(tokens)
		});
		let binds = bindings(&local.pat)
			.iter()
			.map(|local| local.ident.to_string())
			.collect();
		Self { uses, binds }
	}
}

/// Whether `name` is read after a wait, before a later `let` shadows it.
/// `polls[k]` is the wait that ends `segments[k]`.
fn used_later(name: &str, segments: &[Vec<Reads>], polls: &[HashSet<String>]) -> bool {
	for (k, segment) in segments.iter().enumerate() {
		for reads in segment {
			if reads.uses.contains(name) {
				return true;
			}
			if reads.binds.iter().any(|bind| bind == name) {
				return false;
			}
		}
		if polls.get(k).is_some_and(|poll| poll.contains(name)) {
			return true;
		}
	}
	false
}
