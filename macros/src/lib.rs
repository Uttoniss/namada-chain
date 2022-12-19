//! Namada macros for generating WASM binding code for transactions and validity
//! predicates.

#![doc(html_favicon_url = "https://dev.namada.net/master/favicon.png")]
#![doc(html_logo_url = "https://dev.namada.net/master/rustdoc-logo.png")]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]

use proc_macro::TokenStream;
use proc_macro2::{Span as Span2, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, ItemFn, ItemStruct};

/// Generate WASM binding for a transaction main entrypoint function.
///
/// This macro expects a function with signature:
///
/// ```compiler_fail
/// fn apply_tx(
///     ctx: &mut Ctx,
///     tx_data: Vec<u8>
/// ) -> TxResult
/// ```
#[proc_macro_attribute]
pub fn transaction(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as ItemFn);
    let ident = &ast.sig.ident;
    let gen = quote! {
        // Use `wee_alloc` as the global allocator.
        #[global_allocator]
        static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

        #ast

        // The module entrypoint callable by wasm runtime
        #[no_mangle]
        extern "C" fn _apply_tx(tx_data_ptr: u64, tx_data_len: u64) {
            let slice = unsafe {
                core::slice::from_raw_parts(
                    tx_data_ptr as *const u8,
                    tx_data_len as _,
                )
            };
            let tx_data = slice.to_vec();

            // The context on WASM side is only provided by the VM once its
            // being executed (in here it's implicit). But because we want to
            // have interface consistent with the VP interface, in which the
            // context is explicit, in here we're just using an empty `Ctx`
            // to "fake" it.
            let mut ctx = unsafe { namada_tx_prelude::Ctx::new() };

            if let Err(err) = #ident(&mut ctx, tx_data) {
                namada_tx_prelude::debug_log!("Transaction error: {}", err);
                // crash the transaction to abort
                panic!();
            }
        }
    };
    TokenStream::from(gen)
}

/// Generate WASM binding for validity predicate main entrypoint function.
///
/// This macro expects a function with signature:
///
/// ```compiler_fail
/// fn validate_tx(
///     ctx: &Ctx,
///     tx_data: Vec<u8>,
///     addr: Address,
///     keys_changed: BTreeSet<storage::Key>,
///     verifiers: BTreeSet<Address>
/// ) -> VpResult
/// ```
#[proc_macro_attribute]
pub fn validity_predicate(
    _attr: TokenStream,
    input: TokenStream,
) -> TokenStream {
    let ast = parse_macro_input!(input as ItemFn);
    let ident = &ast.sig.ident;
    let gen = quote! {
        // Use `wee_alloc` as the global allocator.
        #[global_allocator]
        static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

        #ast

        // The module entrypoint callable by wasm runtime
        #[no_mangle]
        extern "C" fn _validate_tx(
            // VP's account's address
            addr_ptr: u64,
            addr_len: u64,
            tx_data_ptr: u64,
            tx_data_len: u64,
            keys_changed_ptr: u64,
            keys_changed_len: u64,
            verifiers_ptr: u64,
            verifiers_len: u64,
        ) -> u64 {
            let slice = unsafe {
                core::slice::from_raw_parts(addr_ptr as *const u8, addr_len as _)
            };
            let addr = Address::try_from_slice(slice).unwrap();

            let slice = unsafe {
                core::slice::from_raw_parts(
                    tx_data_ptr as *const u8,
                    tx_data_len as _,
                )
            };
            let tx_data = slice.to_vec();

            let slice = unsafe {
                core::slice::from_raw_parts(
                    keys_changed_ptr as *const u8,
                    keys_changed_len as _,
                )
            };
            let keys_changed: BTreeSet<storage::Key> = BTreeSet::try_from_slice(slice).unwrap();

            let slice = unsafe {
                core::slice::from_raw_parts(
                    verifiers_ptr as *const u8,
                    verifiers_len as _,
                )
            };
            let verifiers: BTreeSet<Address> = BTreeSet::try_from_slice(slice).unwrap();

            // The context on WASM side is only provided by the VM once its
            // being executed (in here it's implicit). But because we want to
            // have interface identical with the native VPs, in which the
            // context is explicit, in here we're just using an empty `Ctx`
            // to "fake" it.
            let ctx = unsafe { namada_vp_prelude::Ctx::new() };

            // run validation with the concrete type(s)
            match #ident(&ctx, tx_data, addr, keys_changed, verifiers)
            {
                Ok(true) => 1,
                Ok(false) => 0,
                Err(err) => {
                    namada_vp_prelude::debug_log!("Validity predicate error: {}", err);
                    0
                },
            }
        }
    };
    TokenStream::from(gen)
}

#[proc_macro_derive(StorageKeys)]
// TODO: use this crate for errors: https://crates.io/crates/proc-macro-error
pub fn derive_storage_keys(item: TokenStream) -> TokenStream {
    let struct_def = parse_macro_input!(item as ItemStruct);

    // type check the struct - all fields must be of type `&'static str`
    let fields = match &struct_def.fields {
        syn::Fields::Named(fields) => &fields.named,
        _ => panic!(
            "Only named struct fields are accepted in StorageKeys derives"
        ),
    };

    let mut idents = vec![];

    for field in fields {
        let field_type = {
            let mut toks = TokenStream2::new();
            field.ty.to_tokens(&mut toks);
            toks.to_string()
        };
        if field_type != "& 'static str" {
            panic!(
                "Expected `&'static str` field type in StorageKeys derive, \
                 but got {field_type} instead"
            );
        }
        idents.push(field.ident.clone().expect("Expected a named field"));
    }

    idents.sort();

    let ident_list = create_ponctuated(&idents, |ident| ident.clone());
    let values_list = create_ponctuated(&idents, |ident| {
        let storage_key = {
            let mut toks = TokenStream2::new();
            ident.to_tokens(&mut toks);
            toks.to_string()
        };
        syn::FieldValue {
            attrs: vec![],
            member: syn::Member::Named(ident.clone()),
            colon_token: Some(syn::token::Colon {
                spans: [Span2::call_site()],
            }),
            expr: syn::Expr::Lit(syn::ExprLit {
                attrs: vec![],
                lit: syn::Lit::Str(syn::LitStr::new(
                    storage_key.as_str(),
                    Span2::call_site(),
                )),
            }),
        }
    });

    let struct_def_ident = &struct_def.ident;

    quote! {
        impl #struct_def_ident {
            const ALL: &[&'static str] = {
                let #struct_def_ident {
                    #ident_list
                } = Self::VALUES;

                &[ #ident_list ]
            };

            const VALUES: #struct_def_ident = Self {
                #values_list
            };
        }
    }
    .into()
}

fn create_ponctuated<F, M>(
    idents: &[syn::Ident],
    mut map: F,
) -> Punctuated<M, syn::token::Comma>
where
    F: FnMut(&syn::Ident) -> M,
{
    idents.iter().fold(Punctuated::new(), |mut accum, ident| {
        accum.push(map(ident));
        accum
    })
}
