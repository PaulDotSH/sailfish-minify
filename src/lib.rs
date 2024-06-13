#![forbid(unsafe_code)]

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use regex::Regex;
use std::fs::{copy, create_dir_all, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{fs, io};
use syn::parse_macro_input;
use syn::ItemStruct;
use syn::Meta;

#[cfg(feature = "minclude")]
static INCLUDE_REPLACE_TOKEN_REGEX: &str = r#"<% *minclude!\("([^"]+)"\); *%>"#;
#[cfg(not(feature = "minclude"))]
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
        let new_file_name = if let Some(ext) = path.extension() {
            let mut file_stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
            file_stem.push_str(".min.");
            file_stem.push_str(ext.to_str().unwrap());
            file_stem
        } else {
            format!("{}.min", file_name.to_str().unwrap())
        };
        new_path.push(new_file_name);
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

fn minify_file(file_path: &Path, new_file_path: &Path, options: &MinifyOptions) {
    if let Some(parent) = new_file_path.parent() {
        fs::create_dir_all(parent).expect("Cannot create directories to minify the file");
    }

    options.minify_file(file_path, new_file_path);
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
            copy(&src_path, &dst_path)?;
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

fn minify_components(path: &Path, minify_options: &MinifyOptions) -> io::Result<()> {
    let mut input_file = File::open(path)?;
    let mut contents = String::new();
    input_file.read_to_string(&mut contents)?;

    let include_regex = Regex::new(INCLUDE_REPLACE_TOKEN_REGEX).unwrap();

    for cap in include_regex.captures_iter(contents.clone().as_str()) {
        let original_str = &cap[0]; // include!("file123")
        let file_name = &cap[1]; // file123

        let new_include = format!(r#"include!("{}/{}")"#, TMP_TEMPLATES_PATH, file_name);

        contents = contents.replace(original_str, &new_include);

        let source_path = format!("./templates/{}", file_name);
        let destination_path = format!("{}/{}", TMP_TEMPLATES_PATH, file_name);
        minify_options.minify_file(Path::new(&source_path), Path::new(&destination_path));

        minify_components(Path::new(&destination_path), minify_options)?;
    }

    Ok(())
}

#[proc_macro_derive(TemplateOnce, attributes(templ, min_with))]
pub fn derive_template_once(tokens: TokenStream) -> TokenStream {
    let token_str = tokens.to_string();

    let file_path = extract_template_path(&token_str);
    let new_path = modify_template_path(&file_path);

    let templates_path = Path::new(TMP_MAIN_PATH).join("./templates");
    copy_dir(Path::new("./templates"), templates_path.as_path()).unwrap();

    let mut minify_options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens.clone(), &mut minify_options);
    minify_file(&file_path, &new_path, &minify_options);
    minify_components(&new_path, &minify_options).unwrap();

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
