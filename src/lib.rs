#![forbid(unsafe_code)]

extern crate proc_macro;

use proc_macro::TokenStream;
use minify::{minify_template, modify_template_path, parse_derive_input, set_template_path, DeriveInput};
use quote::ToTokens;

mod minify;

#[proc_macro_derive(TemplateSimple, attributes(template, min_with))]
pub fn derive_template_simple(tokens: TokenStream) -> TokenStream {
    let DeriveInput { mut item, template_path, minify_options } =
        match parse_derive_input(tokens.into()) {
            Ok(input) => input,
            Err(err) => return err.into_compile_error().into(),
        };
    let new_path = modify_template_path(&template_path);

    if let Err(err) = minify_template(&template_path, &new_path, &minify_options) {
        panic!(
            "sailfish-minify: failed to minify template {:?}: {}",
            template_path, err
        );
    }

    set_template_path(&mut item, new_path.to_str().unwrap());

    let output = sailfish_compiler::procmacro::derive_template_simple(item.into_token_stream());

    TokenStream::from(output)
}
