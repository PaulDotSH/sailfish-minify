#![forbid(unsafe_code)]

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use regex::Regex;
use std::fs::{create_dir_all, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{fs, io};
use syn::parse_macro_input;
use syn::ItemStruct;
use syn::Meta;
static INCLUDE_REPLACE_TOKEN_REGEX: &str = r#"<% *include!\("([^"]+)"\); *%>"#;

static TMP_MAIN_PATH: &str = "/tmp/sailfish-minify";
static TMP_TEMPLATES_PATH: &str = "/tmp/sailfish-minify/templates";

fn replace_path_attribute(input: TokenStream, new_path: &str) -> TokenStream {
    let struct_item = parse_macro_input!(input as ItemStruct);

    let new_path_token = quote! { #[template(path = #new_path)] };

    let expanded = quote! {
        #new_path_token
        #struct_item
    };

    expanded.into()
}

fn modify_template_path(path: &Path) -> PathBuf {
    let mut new_path = PathBuf::from(TMP_MAIN_PATH);

    if let Some(parent) = path.parent() {
        new_path.push(parent);
    }

    if let Some(file_name) = path.file_name() {
        new_path.push(format!("{}.min", file_name.to_str().unwrap()));
    }
    new_path
}

fn extract_template_path(str: &str) -> PathBuf {
    let start_idx = str.find("path = \"").expect("Cannot find path in template") + 8;
    let end_idx = str[start_idx..]
        .find('"')
        .expect("Cannot find path in template")
        + start_idx;
    Path::new("./templates").join(&str[start_idx..end_idx])
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let mut dst_path = PathBuf::from(dst);
        dst_path.push(entry.file_name());

        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
enum Minifier {
    #[default]
    HTMLMinifier,
    Custom(String),
    CustomUnchecked(String),
}

#[derive(Debug, Default)]
struct MinifyOptions {
    minifier: Minifier,
}

fn run_custom_command_unchecked_wrapper(command: &str, input: &Path, output: &Path) -> Output {
    let mut cmd: Vec<&str> = command.split(' ').collect();
    cmd.extend(vec![
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    run_custom_command_unchecked(&cmd)
}

fn run_custom_command_unchecked(cmd: &[&str]) -> Output {
    Command::new(cmd[0])
        .args(cmd.iter().skip(1))
        .output()
        .expect("Failed to run minifier")
}

fn run_custom_command(cmd: &[&str]) -> Output {
    let out = run_custom_command_unchecked(cmd);
    if !out.stderr.is_empty() {
        panic!(
            "Minifier ran with error  {:?}",
            String::from_utf8(out.stderr).unwrap()
        )
    }
    out
}

impl MinifyOptions {
    fn minify_file(&self, input: &Path, output: &Path) {
        match &self.minifier {
            Minifier::HTMLMinifier => {
                run_custom_command(&[
                    "html-minifier",
                    "--collapse-whitespace",
                    input.to_str().unwrap(),
                    "-o",
                    output.to_str().unwrap(),
                ]);
            }
            Minifier::Custom(command) => {
                let out = run_custom_command_unchecked_wrapper(command, input, output);

                if !out.stderr.is_empty() {
                    panic!(
                        "Minifier ran with error  {:?}",
                        String::from_utf8(out.stderr).unwrap()
                    )
                }
            }
            Minifier::CustomUnchecked(command) => {
                run_custom_command_unchecked_wrapper(command, input, output);
            }
        }
    }
}

// Ugly workaround
fn get_minify_options_from_token_stream(
    tokens: TokenStream,
    options: &mut MinifyOptions,
) -> TokenStream {
    let mut struct_item = syn::parse_macro_input!(tokens as ItemStruct);

    for attr in &mut struct_item.attrs {
        let parsed_args = attr.parse_args();

        if attr.path().segments[0].ident == "min_with" {
            if let Ok(Meta::List(nv)) = parsed_args {
                match nv.path.segments[0].ident.to_string().as_str() {
                    "Custom" => { options.minifier = Minifier::Custom(nv.tokens.to_string()) }
                    "CustomUnchecked" => { options.minifier = Minifier::CustomUnchecked(nv.tokens.to_string())}
                    _ => panic!("Wrong minifier value, supported values are HTMLMinifier, Custom/CustomUnchecked(\"command\")")
                }
            } else if let Ok(Meta::Path(nv)) = parsed_args {
                options.minifier = match nv.segments[0].ident.to_string().as_str() {
                    "HTMLMinifier" => Minifier::HTMLMinifier,
                    _ => panic!("Wrong minifier value, supported values are HTMLMinifier, Custom/CustomUnchecked(\"command\")")
                }
            }
        }
    }

    TokenStream::new()
}

fn minify_file_and_components(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
) -> io::Result<()> {
    let mut input_file = File::open(file_path)?;
    let mut contents = String::new();
    input_file.read_to_string(&mut contents)?;
    let include_regex = Regex::new(INCLUDE_REPLACE_TOKEN_REGEX).unwrap();
    for cap in include_regex.captures_iter(contents.clone().as_str()) {
        let original_str = &cap[0]; // include!("file123.stpl")
        let file_name = &cap[1]; // file123.stpl

        let new_file_path = format!(
            "/{}/{}/{}.min",
            TMP_MAIN_PATH,
            file_path.parent().unwrap().to_str().unwrap(),
            file_name
        );

        let new_include = format!(r#"<% include!("{}"); %>"#, new_file_path);
        contents = contents.replace(original_str, &new_include);

        create_dir_all(new_path.parent().unwrap()).expect("Cannot create dir");
        fs::write(new_path, &contents)?;

        let tmp_fp = file_path.parent().unwrap();
        let component_file_path = format!("{}/{}", tmp_fp.to_str().unwrap(), file_name);

        minify_file_and_components(
            component_file_path.as_ref(),
            new_file_path.as_ref(),
            minify_options,
        ).expect("Couldn't minify a component :(");
    }
    create_dir_all(new_path.parent().unwrap()).expect("Cannot create dir");
    fs::write(new_path, contents)?;
    minify_options.minify_file(new_path, new_path);
    Ok(())
}

#[proc_macro_derive(TemplateOnce, attributes(templ, min_with))]
pub fn derive_template_once(tokens: TokenStream) -> TokenStream {
    let token_str = tokens.to_string();

    let file_path = extract_template_path(&token_str);
    let new_path = modify_template_path(&file_path);

    let mut minify_options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens.clone(), &mut minify_options);

    #[cfg(feature = "minify-components")]
    minify_file_and_components(&file_path, &new_path, &minify_options).unwrap();
    #[cfg(not(feature = "minify-components"))]
    {
        copy_dir(Path::new("./templates"), Path::new(TMP_TEMPLATES_PATH));
        minify_options.minify_file(&file_path, &new_path);
    }

    let input = replace_path_attribute(tokens, new_path.to_str().unwrap());

    let input = proc_macro2::TokenStream::from(input);

    let output = sailfish_compiler::procmacro::derive_template(input);
    // fs::remove_file(new_path);
    TokenStream::from(output)
}

/// WIP
#[proc_macro_derive(Template, attributes(template, min_with))]
pub fn derive_template(tokens: TokenStream) -> TokenStream {
    let input = proc_macro2::TokenStream::from(tokens);
    let output = sailfish_compiler::procmacro::derive_template(input);
    TokenStream::from(output)
}
