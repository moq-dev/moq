// Appended only to the snapshot's existing kio-macros crate.
#[proc_macro_attribute]
pub fn wrapper(
    args: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    fn expand(
        args: proc_macro2::TokenStream,
        method: syn::ImplItemFn,
    ) -> syn::Result<proc_macro2::TokenStream> {
        if !args.is_empty() {
            return Err(syn::Error::new_spanned(args, "wrapper takes no arguments"));
        }
        let sig = &method.sig;
        let name = sig.ident.to_string();
        let Some(name) = name.strip_prefix("poll_").filter(|name| !name.is_empty()) else {
            return Err(syn::Error::new_spanned(
                &sig.ident,
                "wrapper requires a poll_* method",
            ));
        };
        let Some(syn::FnArg::Receiver(receiver)) = sig.inputs.first() else {
            return Err(syn::Error::new_spanned(
                &sig.inputs,
                "wrapper requires a borrowed receiver",
            ));
        };
        if receiver.reference.is_none() || receiver.colon_token.is_some() {
            return Err(syn::Error::new_spanned(
                receiver,
                "wrapper requires a borrowed receiver",
            ));
        }
        let Some(syn::FnArg::Typed(waiter)) = sig.inputs.iter().nth(1) else {
            return Err(syn::Error::new_spanned(
                &sig.inputs,
                "wrapper requires &Waiter after self",
            ));
        };
        let syn::Type::Reference(waiter_ref) = waiter.ty.as_ref() else {
            return Err(syn::Error::new_spanned(
                &waiter.ty,
                "wrapper requires &Waiter after self",
            ));
        };
        let syn::Type::Path(waiter_path) = waiter_ref.elem.as_ref() else {
            return Err(syn::Error::new_spanned(
                &waiter.ty,
                "wrapper requires &Waiter after self",
            ));
        };
        if waiter_ref.mutability.is_some()
            || !waiter_path.path.is_ident("Waiter")
                && waiter_path
                    .path
                    .segments
                    .last()
                    .is_none_or(|segment| segment.ident != "Waiter")
        {
            return Err(syn::Error::new_spanned(
                &waiter.ty,
                "wrapper requires &Waiter after self",
            ));
        }
        let syn::ReturnType::Type(_, ty) = &sig.output else {
            return Err(syn::Error::new_spanned(
                &sig.output,
                "wrapper requires Poll<T>",
            ));
        };
        let syn::Type::Path(path) = ty.as_ref() else {
            return Err(syn::Error::new_spanned(ty, "wrapper requires Poll<T>"));
        };
        let segment = path.path.segments.last().unwrap();
        let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
            return Err(syn::Error::new_spanned(ty, "wrapper requires Poll<T>"));
        };
        let Some(syn::GenericArgument::Type(output)) = arguments
            .args
            .first()
            .filter(|_| arguments.args.len() == 1 && segment.ident == "Poll")
        else {
            return Err(syn::Error::new_spanned(ty, "wrapper requires Poll<T>"));
        };
        // This checkpoint deliberately refuses more than the receiver and waiter.
        if sig.inputs.len() != 2
            || !sig.generics.params.is_empty()
            || sig.asyncness.is_some()
            || sig.unsafety.is_some()
        {
            return Err(syn::Error::new_spanned(
                sig,
                "spike supports safe, non-generic methods with only self and waiter",
            ));
        }
        let mut wrapper = sig.clone();
        wrapper.ident = syn::Ident::new(name, sig.ident.span());
        wrapper.asyncness = Some(Default::default());
        wrapper.inputs = std::iter::once(sig.inputs.first().unwrap().clone()).collect();
        wrapper.output = syn::parse_quote!(-> #output);
        let docs = method
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("doc"));
        let vis = &method.vis;
        let poll = &sig.ident;
        Ok(quote! {
            #method
            #(#docs)*
            #[cfg(feature = "async")]
            #vis #wrapper { ::kio::wait(|waiter| self.#poll(waiter)).await }
        })
    }
    let method = syn::parse_macro_input!(item as syn::ImplItemFn);
    expand(args.into(), method)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
