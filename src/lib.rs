#![forbid(unsafe_code)]

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use rayon::prelude::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs::{create_dir_all, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};
use std::{fs, io};
use syn::parse_macro_input;
use syn::ItemStruct;
use syn::Meta;
static INCLUDE_REPLACE_TOKEN_REGEX: &str = r#"<% *include!\("([^"]+)"\); *%>"#;

static TMP_MAIN_PATH: &str = "/tmp/sailfish-minify";

// Global cache to track processed components across all template compilations
static GLOBAL_PROCESSED_CACHE: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();

fn get_global_cache() -> &'static Mutex<HashMap<PathBuf, PathBuf>> {
    GLOBAL_PROCESSED_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn replace_path_attribute(input: TokenStream, new_path: &str) -> TokenStream {
    let mut struct_item = parse_macro_input!(input as ItemStruct);

    // Find and replace the template attribute
    for attr in &mut struct_item.attrs {
        if attr.path().is_ident("template") {
            // Replace the entire attribute with the new path
            let new_attr = syn::parse_quote! { #[template(path = #new_path)] };
            *attr = new_attr;
            break;
        }
    }

    let expanded = quote! {
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
    // Look for #[template(path = "...")]
    let template_regex = regex::Regex::new(r#"#\[template\([^)]*path\s*=\s*"([^"]+)"[^)]*\)\]"#).unwrap();
    
    if let Some(captures) = template_regex.captures(str) {
        let path = captures.get(1).expect("Cannot find path in template").as_str();
        Path::new("./templates").join(path)
    } else {
        panic!("Cannot find template path in struct attributes. Make sure to use #[template(path = \"...\")] for minified templates");
    }
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

fn minify_file_and_components_internal(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
    processed_files: Arc<Mutex<HashSet<PathBuf>>>,
) -> io::Result<()> {
    let canonical_path = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
    
    // Check local cache first (for same template)
    {
        let processed = processed_files.lock().unwrap();
        if processed.contains(&canonical_path) {
            return Ok(());
        }
    }
    
    {
        let global_cache = get_global_cache().lock().unwrap();
        if let Some(cached_output_path) = global_cache.get(&canonical_path) {
            if cached_output_path.exists() && new_path != cached_output_path {
                create_dir_all(new_path.parent().unwrap()).expect("Cannot create dir");
                fs::copy(cached_output_path, new_path)?;
            }
            processed_files.lock().unwrap().insert(canonical_path);
            return Ok(());
        }
    }
    
    processed_files.lock().unwrap().insert(canonical_path.clone());

    let mut input_file = File::open(file_path)?;
    let mut contents = String::new();
    input_file.read_to_string(&mut contents)?;
    let include_regex = Regex::new(INCLUDE_REPLACE_TOKEN_REGEX).unwrap();
    
    let includes: Vec<_> = include_regex.captures_iter(&contents).collect();
    
    if !includes.is_empty() {
        let include_results: Vec<Result<(String, String), Box<dyn std::error::Error + Send + Sync>>> = includes.par_iter().map(|cap| {
            let original_str = cap[0].to_string(); // include!("file123.stpl")
            let file_name = cap[1].to_string(); // file123.stpl

            let new_file_path = format!(
                "/{}/{}/{}.min",
                TMP_MAIN_PATH,
                file_path.parent().unwrap().to_str().unwrap(),
                file_name
            );

            let tmp_fp = file_path.parent().unwrap();
            let component_file_path = format!("{}/{}", tmp_fp.to_str().unwrap(), file_name);

            minify_file_and_components_internal(
                component_file_path.as_ref(),
                new_file_path.as_ref(),
                minify_options,
                Arc::clone(&processed_files),
            ).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            
            Ok((original_str, new_file_path))
        }).collect();

        for result in include_results {
            match result {
                Ok((original_str, new_file_path)) => {
                    let new_include = format!(r#"<% include!("{}"); %>"#, new_file_path);
                    contents = contents.replace(&original_str, &new_include);
                }
                Err(e) => {
                    eprintln!("Error processing component: {}", e);
                    return Err(io::Error::new(io::ErrorKind::Other, "Component processing failed"));
                }
            }
        }
    }
    
    create_dir_all(new_path.parent().unwrap()).expect("Cannot create dir");
    fs::write(new_path, contents)?;
    minify_options.minify_file(new_path, new_path);
    
    {
        let mut global_cache = get_global_cache().lock().unwrap();
        global_cache.insert(canonical_path, new_path.to_path_buf());
    }
    
    Ok(())
}

fn minify_file_and_components(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
) -> io::Result<()> {
    let processed_files = Arc::new(Mutex::new(HashSet::new()));
    minify_file_and_components_internal(file_path, new_path, minify_options, processed_files)
}

#[proc_macro_derive(TemplateSimple, attributes(template, min_with))]
pub fn derive_template_simple(tokens: TokenStream) -> TokenStream {
    let token_str = tokens.to_string();

    let file_path = extract_template_path(&token_str);
    let new_path = modify_template_path(&file_path);

    let mut minify_options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens.clone(), &mut minify_options);

    #[cfg(feature = "minify-components")]
    minify_file_and_components(&file_path, &new_path, &minify_options).unwrap();
    #[cfg(not(feature = "minify-components"))]
    {
        copy_dir(Path::new("./templates"), &PathBuf::from(TMP_MAIN_PATH).join("templates"));
        minify_options.minify_file(&file_path, &new_path);
    }

    let input = replace_path_attribute(tokens, new_path.to_str().unwrap());

    let input = proc_macro2::TokenStream::from(input);

    let output = sailfish_compiler::procmacro::derive_template_simple(input);

    TokenStream::from(output)
}

#[allow(dead_code)]
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

// /// WIP
// #[proc_macro_derive(Template, attributes(template, min_with))]
// pub fn derive_template(tokens: TokenStream) -> TokenStream {
//     let input = proc_macro2::TokenStream::from(tokens);
//     let output = sailfish_compiler::procmacro::derive_template(input);
//     TokenStream::from(output)
// }
