use quote::quote;
use sailfish_compiler::Config;
use rayon::prelude::*;
#[cfg(feature = "regex")]
use regex::Regex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{create_dir_all, File};
use std::io::Read;
use std::ops::Range;
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::{fs, io};
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, ExprLit, ItemStruct, Lit, Meta, Token};

#[cfg(feature = "regex")]
const INCLUDE_REPLACE_TOKEN_REGEX: &str = r#"<% *include!\s*\(\s*"([^"]+)"\s*\)\s*;\s*%>"#;

// Cache Regex compilation
#[cfg(feature = "regex")]
static INCLUDE_REGEX_CACHE: OnceLock<Regex> = OnceLock::new();

// Global cache to track processed components across all template compilations
//
// Only used when `minify-components` is enabled, but kept compiled in every
// configuration because the unit tests in `sailfish-minify-core` exercise it
// either way.
#[allow(dead_code)]
static GLOBAL_PROCESSED_CACHE: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();

// Used by the unit tests to verify cache behavior.
#[doc(hidden)]
pub static MINIFIER_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[allow(dead_code)]
fn get_global_cache() -> &'static Mutex<HashMap<PathBuf, PathBuf>> {
    GLOBAL_PROCESSED_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(feature = "regex")]
pub fn get_include_regex() -> &'static Regex {
    INCLUDE_REGEX_CACHE.get_or_init(|| Regex::new(INCLUDE_REPLACE_TOKEN_REGEX).unwrap())
}

/// Base directory where minified templates are written.
static TMP_MAIN_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Base directory where minified templates are written.
///
/// Cached in a `OnceLock` so the `temp_dir()` syscall and `PathBuf` allocation
/// happen only once per process.
pub fn tmp_main_path() -> &'static Path {
    TMP_MAIN_PATH.get_or_init(|| std::env::temp_dir().join("sailfish-minify"))
}

/// Cache of resolved CLI tool paths on Windows, keyed by bare command name.
///
/// `resolve_command` is called once per minification; the PATH search (with
/// `.exe`/`.cmd`/`.bat`/`.ps1` candidate probes) only needs to run the first
/// time each command is seen.
#[cfg(windows)]
static RESOLVED_COMMANDS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve the name of a CLI tool to an absolute path on Windows.
///
/// `html-minifier` is installed through npm, which only creates `*.cmd` /
/// `*.ps1` shims. Rust's `std::process::Command` cannot execute those by bare
/// name, so we search `PATH` and hand back the full path instead.
pub fn resolve_command(name: &str) -> String {
    #[cfg(windows)]
    {
        let mut cache = RESOLVED_COMMANDS.lock().unwrap();
        if let Some(path) = cache.get(name) {
            return path.clone();
        }
        if let Some(path) = find_executable_in_path(name) {
            cache.insert(name.to_string(), path.clone());
            return path;
        }
    }
    name.to_string()
}

#[cfg(windows)]
fn find_executable_in_path(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        for ext in ["exe", "cmd", "bat", "ps1"] {
            let candidate = dir.join(format!("{name}.{ext}"));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Everything `#[derive(TemplateSimple)]` needs from its input, read from a
/// single `syn` parse of the struct.
pub struct DeriveInput {
    pub item: ItemStruct,
    /// Source template, from `#[template(path = "...")]`.
    pub template_path: PathBuf,
    /// Minifier selection, from `#[min_with(...)]`.
    pub minify_options: MinifyOptions,
}

/// Parse the derive input once and read the template path and minifier
/// options from its attributes, with the crate's `sailfish.toml` settings.
pub fn parse_derive_input(tokens: proc_macro2::TokenStream) -> syn::Result<DeriveInput> {
    let item = syn::parse2::<ItemStruct>(tokens)?;
    let manifest_dir = manifest_dir();
    let config = sailfish_config(&manifest_dir)?;
    let template_path = template_path_from_attrs(&item.attrs, &config.template_dirs, &manifest_dir)?;
    let minify_options = minify_options_from_attrs(&item.attrs, config.delimiter)?;
    Ok(DeriveInput { item, template_path, minify_options })
}

/// Directory of the crate being compiled, the one using the derive.
fn manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("environment variable `CARGO_MANIFEST_DIR` is not set"),
    )
}

/// The `sailfish.toml` settings for the crate in `manifest_dir`, read the
/// same way the sailfish derive reads them.
fn sailfish_config(manifest_dir: &Path) -> syn::Result<Config> {
    Config::search_file_and_read(manifest_dir)
        .map_err(|err| syn::Error::new(proc_macro2::Span::call_site(), err))
}

/// Extract the template file path from a `#[template(path = "...")]` attribute
/// in the source of a struct, resolved as in `resolve_template_path` against
/// the `sailfish.toml` settings of the crate being compiled.
///
/// The derive uses `parse_derive_input`; this is kept for the unit tests in
/// `sailfish-minify-core`.
#[allow(dead_code)]
pub fn extract_template_path(str: &str) -> syn::Result<PathBuf> {
    let item = syn::parse_str::<ItemStruct>(str)?;
    let manifest_dir = manifest_dir();
    let config = sailfish_config(&manifest_dir)?;
    template_path_from_attrs(&item.attrs, &config.template_dirs, &manifest_dir)
}

fn template_path_from_attrs(
    attrs: &[Attribute],
    template_dirs: &[PathBuf],
    manifest_dir: &Path,
) -> syn::Result<PathBuf> {
    let Some(Lit::Str(path)) = template_attr_lit(attrs, "path") else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "Cannot find template path in struct attributes. Make sure to use #[template(path = \"...\")] for minified templates",
        ));
    };
    Ok(resolve_template_path(&path.value(), template_dirs, manifest_dir))
}

/// Find a template file the way sailfish does: in each of `template_dirs`
/// (from `sailfish.toml`, in sailfish's lookup order, so last first), then in
/// `<manifest_dir>/templates`. An absolute `path` is used as-is.
///
/// A file found nowhere resolves to `<manifest_dir>/templates/<path>`, so
/// the error from reading it names the default location.
pub fn resolve_template_path(path: &str, template_dirs: &[PathBuf], manifest_dir: &Path) -> PathBuf {
    let fallback = manifest_dir.join("templates").join(path);
    template_dirs
        .iter()
        .rev()
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
        .unwrap_or(fallback)
}

/// The literal value of a `key = ...` option in the struct's `#[template(...)]`
/// attributes, if any.
fn template_attr_lit(attrs: &[Attribute], key: &str) -> Option<Lit> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("template"))
        .find_map(|attr| {
            let args = attr
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .ok()?;
            args.into_iter().find_map(|meta| match meta {
                Meta::NameValue(nv) if nv.path.is_ident(key) => match nv.value {
                    Expr::Lit(ExprLit { lit, .. }) => Some(lit),
                    _ => None,
                },
                _ => None,
            })
        })
}

/// Compute the minified output path for a source template: its full path
/// mirrored under `tmp_main_path()` (see `mirror_path`), with a `.min` suffix
/// on the file name.
pub fn modify_template_path(path: &Path) -> PathBuf {
    let mut new_path = mirror_path(tmp_main_path(), path).into_os_string();
    new_path.push(".min");
    new_path.into()
}

/// Where `path` goes under `base`: its absolute path, with `.` and `..`
/// resolved, recreated beneath `base` (`C:\app\a.stpl` becomes
/// `base\C\app\a.stpl`, `/app/a.stpl` becomes `base/app/a.stpl`).
///
/// Using the full path keeps the files of different crates that share a
/// relative template path apart, keeps absolute template paths inside `base`,
/// and preserves the relative paths between a template and its includes.
pub fn mirror_path(base: &Path, path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut mirrored = base.to_path_buf();
    // Path parts after the root; `..` pops one, never reaching above the root.
    let mut parts: Vec<&OsStr> = Vec::new();

    for component in absolute.components() {
        match component {
            // A Windows prefix only ever comes first.
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    mirrored.push(char::from(letter).to_string());
                }
                Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                    mirrored.extend([OsStr::new("UNC"), server, share]);
                }
                Prefix::Verbatim(name) | Prefix::DeviceNS(name) => mirrored.push(name),
            },
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(name) => parts.push(name),
        }
    }

    mirrored.extend(parts);
    mirrored
}

/// Rewrite the `#[template(path = "...")]` attribute to point at the minified file.
///
/// The derive uses `set_template_path`; this is kept for the unit tests in
/// `sailfish-minify-core`.
#[allow(dead_code)]
pub fn replace_path_attribute(input: proc_macro2::TokenStream, new_path: &str) -> proc_macro2::TokenStream {
    let mut struct_item = syn::parse2::<ItemStruct>(input).unwrap_or_else(|_| {
        panic!("sailfish-minify: expected a struct for #[derive(TemplateSimple)]")
    });
    set_template_path(&mut struct_item, new_path);

    quote! {
        #struct_item
    }
}

/// Point the struct's `#[template(...)]` attribute at `new_path`, keeping its
/// other sailfish options (`escape`, `delimiter`, `rm_whitespace`, ...).
///
/// Rewrites the same attribute `template_path_from_attrs` reads: the first
/// `#[template]` with a `path` key.
pub fn set_template_path(item: &mut ItemStruct, new_path: &str) {
    for attr in item.attrs.iter_mut().filter(|attr| attr.path().is_ident("template")) {
        let Ok(mut args) = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        else {
            continue;
        };
        let Some(path_value) = args.iter_mut().find_map(|meta| match meta {
            Meta::NameValue(nv) if nv.path.is_ident("path") => Some(&mut nv.value),
            _ => None,
        }) else {
            continue;
        };
        *path_value = syn::parse_quote!(#new_path);
        *attr = syn::parse_quote! { #[template(#args)] };
        return;
    }
}

/// Extract the file names referenced by `<% include!("..."); %>` macros.
///
/// Only used by the `copy_referenced_template_and_includes` path (compiled when
/// `minify-components` is disabled) and by the unit tests in
/// `sailfish-minify-core`.
#[allow(dead_code)]
pub fn extract_includes(contents: &str) -> Vec<String> {
    extract_include_ranges(contents, DEFAULT_DELIMITER)
        .into_iter()
        .map(|(_, _, file_name)| file_name)
        .collect()
}

/// Extract every `<% include!("..."); %>` occurrence (with `%` replaced by
/// `delimiter`) as `(byte_range, full_match, file_name)` triples, in source
/// order. The byte range covers the full `include!` token, so callers can
/// splice replacements back into the source without re-scanning it.
///
/// Uses the `regex` engine for the default delimiter when the `regex` feature
/// is enabled, and the dependency-free manual parser otherwise.
pub fn extract_include_ranges(
    contents: &str,
    delimiter: char,
) -> Vec<(Range<usize>, String, String)> {
    #[cfg(feature = "regex")]
    if delimiter == DEFAULT_DELIMITER {
        return get_include_regex()
            .captures_iter(contents)
            .map(|cap| {
                let m = cap.get(0).unwrap();
                (m.range(), cap[0].to_string(), include_arg_value(&cap[1]))
            })
            .collect();
    }
    extract_includes_manual_ranges(contents, delimiter)
}

/// Extract every `<% include!("..."); %>` occurrence as `(full_match, file_name)`
/// pairs, in source order.
///
/// Reachable from the unit tests and benchmarks even when the `regex` feature
/// is enabled, hence `dead_code`.
#[allow(dead_code)]
pub fn extract_includes_spans(contents: &str) -> Vec<(String, String)> {
    extract_include_ranges(contents, DEFAULT_DELIMITER)
        .into_iter()
        .map(|(_, full_match, file_name)| (full_match, file_name))
        .collect()
}

/// Dependency-free manual parser for the same pattern as
/// `INCLUDE_REPLACE_TOKEN_REGEX` (`<% *include!\s*\(\s*"([^"]+)"\s*\)\s*;\s*%>`).
///
/// Returns `(full_match, file_name)` pairs in source order.
///
/// Reachable from the `copy_referenced_template_and_includes` path and the
/// benchmarks even when the `regex` feature is enabled, hence `dead_code`.
#[allow(dead_code)]
pub fn extract_includes_manual(contents: &str) -> Vec<(String, String)> {
    extract_includes_manual_ranges(contents, DEFAULT_DELIMITER)
        .into_iter()
        .map(|(_, full_match, file_name)| (full_match, file_name))
        .collect()
}

/// Dependency-free manual parser returning `(byte_range, full_match, file_name)`
/// triples in source order.
///
/// `full_match` is kept alongside the byte range so callers that only need the
/// text (e.g. the unit tests) can share this parser.
fn extract_includes_manual_ranges(
    contents: &str,
    delimiter: char,
) -> Vec<(Range<usize>, String, String)> {
    let (open, close) = block_delimiters(delimiter);
    let mut results = Vec::new();
    let mut pos = 0;

    while let Some(rel) = contents[pos..].find(&*open) {
        let start = pos + rel;
        let mut rest = &contents[start + open.len()..];

        // `rest` stays a slice of `contents` throughout, so the parsed file
        // name remains valid once the block returns.
        let file_name = 'parse: {
            rest = rest.trim_start_matches(' ');
            if !rest.starts_with("include!") {
                break 'parse None;
            }
            rest = &rest["include!".len()..];

            rest = rest.trim_start();
            if !rest.starts_with('(') {
                break 'parse None;
            }
            rest = &rest[1..];

            rest = rest.trim_start();
            if !rest.starts_with('"') {
                break 'parse None;
            }
            rest = &rest[1..];

            let Some(quote_end) = rest.find('"') else { break 'parse None };
            let name = &rest[..quote_end];
            if name.is_empty() {
                break 'parse None;
            }
            rest = &rest[quote_end + 1..];

            rest = rest.trim_start();
            if !rest.starts_with(')') {
                break 'parse None;
            }
            rest = &rest[1..];

            rest = rest.trim_start();
            if !rest.starts_with(';') {
                break 'parse None;
            }
            rest = &rest[1..];

            rest = rest.trim_start();
            if !rest.starts_with(&*close) {
                break 'parse None;
            }

            Some(name)
        };

        if let Some(file_name) = file_name {
            // `rest` currently points at the closing delimiter; its absolute
            // offset in `contents` is `contents.len() - rest.len()`, so the
            // match ends just past it.
            let end = contents.len() - rest.len() + close.len();
            results.push((start..end, contents[start..end].to_string(), include_arg_value(file_name)));
            pos = end;
        } else {
            pos = start + open.len();
        }
    }

    results
}

/// The file name an `include!("...")` argument refers to. Sailfish reads the
/// argument as a Rust string literal, so `"sub\\part.stpl"` (as Windows users
/// may write it) means `sub\part.stpl`.
fn include_arg_value(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_owned();
    }
    syn::parse_str::<syn::LitStr>(&format!("\"{raw}\""))
        .map(|lit| lit.value())
        .unwrap_or_else(|_| raw.to_owned())
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum Minifier {
    /// Minify with the `html-minifier` CLI (must be installed and on `PATH`).
    HTMLMinifier,
    Custom(String),
    CustomUnchecked(String),
    /// Pure-Rust minifier (`minify-html`), enabled by the `native-minifier`
    /// feature. No `Command` spawn, no system dependency, WASM-compatible.
    /// This is the default minifier.
    Native,
}

/// Build the `minify-html` configuration used by `Minifier::Native`.
///
/// Based on the spec-compliant preset so the output is safe, but tuned for
/// sailfish templates and for parity with the default `html-minifier
/// --collapse-whitespace` invocation: `<% ... %>` blocks are passed through
/// untouched, comments are kept, and optional closing tags are not omitted.
#[cfg(feature = "native-minifier")]
pub fn native_minify_cfg() -> minify_html::Cfg {
    let mut cfg = minify_html::Cfg::new();
    cfg.preserve_chevron_percent_template_syntax = true;
    cfg.keep_comments = true;
    cfg.keep_closing_tags = true;
    cfg
}

#[derive(Debug, Clone, PartialEq)]
pub struct MinifyOptions {
    pub minifier: Minifier,
    /// Sailfish block delimiter, from `#[template(delimiter = '...')]` or
    /// `sailfish.toml`.
    pub delimiter: char,
}

/// Sailfish's default block delimiter (`<% ... %>`), the only one minify-html
/// and html-minifier know to leave untouched.
pub const DEFAULT_DELIMITER: char = '%';

/// The `<D` and `D>` that open and close a sailfish block. Borrowed for the
/// default delimiter, so the common path never allocates.
fn block_delimiters(delimiter: char) -> (Cow<'static, str>, Cow<'static, str>) {
    if delimiter == DEFAULT_DELIMITER {
        (Cow::Borrowed("<%"), Cow::Borrowed("%>"))
    } else {
        (format!("<{delimiter}").into(), format!("{delimiter}>").into())
    }
}

/// Start and end of the `<% ... %>` placeholders that stand in for
/// custom-delimiter blocks while a minifier runs. The spaces make minifiers
/// treat them like typical `<%= value %>` blocks (e.g. keeping attribute
/// quotes).
const PLACEHOLDER_OPEN: &str = "<% sailfish-minify:";
const PLACEHOLDER_CLOSE: &str = " %>";

/// Swap every `<D ... D>` block and `<DD` escape (`D` = `delimiter`) for a
/// numbered `<% ... %>` placeholder, which both minifiers pass through.
///
/// Returns the protected source and each block's byte range in `source`, for
/// `restore_blocks`. Like the default-delimiter path, a block ends at the first
/// closing delimiter; an unterminated block is left for sailfish to report.
pub fn protect_blocks(source: &str, delimiter: char) -> io::Result<(String, Vec<Range<usize>>)> {
    if source.contains(PLACEHOLDER_OPEN) {
        return Err(io::Error::other(format!(
            "template contains the reserved text {PLACEHOLDER_OPEN:?}"
        )));
    }

    let (open, close) = block_delimiters(delimiter);
    let mut protected = String::with_capacity(source.len());
    let mut blocks = Vec::new();
    let mut pos = 0;

    while let Some(rel) = source[pos..].find(&*open) {
        let start = pos + rel;
        let body = start + open.len();
        let end = if source[body..].starts_with(delimiter) {
            // `<DD` is sailfish's escape for a literal `<D`.
            body + delimiter.len_utf8()
        } else if let Some(rel) = source[body..].find(&*close) {
            body + rel + close.len()
        } else {
            break;
        };

        protected.push_str(&source[pos..start]);
        protected.push_str(PLACEHOLDER_OPEN);
        protected.push_str(&blocks.len().to_string());
        protected.push_str(PLACEHOLDER_CLOSE);
        blocks.push(start..end);
        pos = end;
    }
    protected.push_str(&source[pos..]);

    Ok((protected, blocks))
}

/// Put the original blocks from `source` back in place of the placeholders
/// `protect_blocks` left in `minified`.
pub fn restore_blocks(minified: &str, source: &str, blocks: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(minified.len());
    let mut pos = 0;

    while let Some(rel) = minified[pos..].find(PLACEHOLDER_OPEN) {
        let start = pos + rel;
        let digits = start + PLACEHOLDER_OPEN.len();
        let digits_end =
            digits + minified[digits..].bytes().take_while(u8::is_ascii_digit).count();
        let block = minified[digits..digits_end]
            .parse::<usize>()
            .ok()
            .and_then(|index| blocks.get(index))
            .filter(|_| minified[digits_end..].starts_with(PLACEHOLDER_CLOSE));

        // `protect_blocks` refuses sources containing the placeholder text, so
        // a malformed one could only come from a minifier rewriting it; leave
        // it as is.
        match block {
            Some(range) => {
                out.push_str(&minified[pos..start]);
                out.push_str(&source[range.clone()]);
                pos = digits_end + PLACEHOLDER_CLOSE.len();
            }
            None => {
                out.push_str(&minified[pos..digits]);
                pos = digits;
            }
        }
    }
    out.push_str(&minified[pos..]);

    out
}

impl Default for MinifyOptions {
    fn default() -> Self {
        MinifyOptions {
            // The `Native` minifier is the default when the `native-minifier`
            // feature is enabled. Without it (e.g. `--no-default-features`),
            // fall back to the `html-minifier` CLI so templates still minify.
            #[cfg(feature = "native-minifier")]
            minifier: Minifier::Native,
            #[cfg(not(feature = "native-minifier"))]
            minifier: Minifier::HTMLMinifier,
            delimiter: DEFAULT_DELIMITER,
        }
    }
}

fn run_custom_command_unchecked(cmd: &[&str]) -> Output {
    let program = resolve_command(cmd[0]);
    Command::new(program)
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

fn run_custom_command_unchecked_wrapper(command: &str, input: &Path, output: &Path) -> Output {
    let mut cmd: Vec<&str> = command.split([' ', '\n']).collect();
    cmd.extend(vec![
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    run_custom_command_unchecked(&cmd)
}

/// Extract the argument of a `Custom(...)` / `CustomUnchecked(...)` minifier.
///
/// Accepts both a quoted string literal (`Custom("cmd --flag")`) and bare
/// tokens (`Custom(cmd --flag)`).
fn parse_command_arg(tokens: &proc_macro2::TokenStream) -> String {
    if let Ok(lit) = syn::parse2::<syn::LitStr>(tokens.clone()) {
        lit.value()
    } else {
        tokens.to_string()
    }
}

impl MinifyOptions {
    pub fn minify_file(&self, input: &Path, output: &Path) -> io::Result<()> {
        MINIFIER_INVOCATIONS.fetch_add(1, Ordering::SeqCst);

        if self.minifier == Minifier::Native {
            #[cfg(feature = "native-minifier")]
            {
                let source = fs::read_to_string(input)?;
                return fs::write(output, self.minify_native(&source)?);
            }
            #[cfg(not(feature = "native-minifier"))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the `Native` minifier requires the `native-minifier` feature",
            ));
        }

        if self.delimiter == DEFAULT_DELIMITER {
            return self.run_cli(input, output);
        }

        // CLI minifiers only skip `<% %>` blocks, so run them on a copy with
        // the custom-delimiter blocks swapped for placeholders.
        let source = fs::read_to_string(input)?;
        let (protected, blocks) = protect_blocks(&source, self.delimiter)?;
        fs::write(output, protected)?;
        self.run_cli(output, output)?;
        let minified = fs::read_to_string(output)?;
        fs::write(output, restore_blocks(&minified, &source, &blocks))
    }

    /// Minify `source` in memory with `minify-html`.
    #[cfg(feature = "native-minifier")]
    pub fn minify_native(&self, source: &str) -> io::Result<Vec<u8>> {
        if self.delimiter == DEFAULT_DELIMITER {
            return Ok(minify_html::minify(source.as_bytes(), &native_minify_cfg()));
        }

        let (protected, blocks) = protect_blocks(source, self.delimiter)?;
        let minified = minify_html::minify(protected.as_bytes(), &native_minify_cfg());
        let minified = String::from_utf8(minified).map_err(io::Error::other)?;
        Ok(restore_blocks(&minified, source, &blocks).into_bytes())
    }

    /// Run a CLI minifier on `input`, writing to `output`.
    fn run_cli(&self, input: &Path, output: &Path) -> io::Result<()> {
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
            Minifier::Native => unreachable!("`minify_file` handles the native minifier"),
        }
        Ok(())
    }
}

/// Parse `#[min_with(...)]` attributes from a struct's token stream and apply
/// them to the given `MinifyOptions`.
///
/// The derive uses `parse_derive_input`; this is kept for the unit tests in
/// `sailfish-minify-core`.
#[allow(dead_code)]
pub fn get_minify_options_from_token_stream(
    tokens: proc_macro2::TokenStream,
    options: &mut MinifyOptions,
) -> syn::Result<()> {
    let struct_item = syn::parse2::<ItemStruct>(tokens)?;
    apply_minify_attrs(&struct_item.attrs, options)
}

/// `default_delimiter` is the `sailfish.toml` delimiter, which
/// `#[template(delimiter = '...')]` overrides.
fn minify_options_from_attrs(attrs: &[Attribute], default_delimiter: char) -> syn::Result<MinifyOptions> {
    let mut options = MinifyOptions { delimiter: default_delimiter, ..MinifyOptions::default() };
    apply_minify_attrs(attrs, &mut options)?;
    Ok(options)
}

fn apply_minify_attrs(attrs: &[Attribute], options: &mut MinifyOptions) -> syn::Result<()> {
    if let Some(Lit::Char(delimiter)) = template_attr_lit(attrs, "delimiter") {
        options.delimiter = delimiter.value();
    }

    for attr in attrs {
        if !attr.path().is_ident("min_with") {
            continue;
        }

        let parsed_args = attr.parse_args()?;
        match parsed_args {
            Meta::List(nv) => {
                let kind = nv.path.segments.first().unwrap().ident.to_string();
                let inner = parse_command_arg(&nv.tokens);
                match kind.as_str() {
                    "Custom" => options.minifier = Minifier::Custom(inner),
                    "CustomUnchecked" => options.minifier = Minifier::CustomUnchecked(inner),
                    "Native" => options.minifier = Minifier::Native,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "Wrong minifier value, supported values are HTMLMinifier, Native, Custom/CustomUnchecked(\"command\")",
                        ))
                    }
                }
            }
            Meta::Path(nv) => {
                let ident = nv.segments.first().unwrap().ident.to_string();
                match ident.as_str() {
                    "HTMLMinifier" => options.minifier = Minifier::HTMLMinifier,
                    "Native" => options.minifier = Minifier::Native,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "Wrong minifier value, supported values are HTMLMinifier, Native, Custom/CustomUnchecked(\"command\")",
                        ))
                    }
                }
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "Wrong minifier value, supported values are HTMLMinifier, Native, Custom/CustomUnchecked(\"command\")",
                ))
            }
        }
    }

    Ok(())
}

/// Apply include rewrites to `contents` in a single pass.
///
/// `replacements` holds `(byte_range, replacement)` pairs, each range spanning
/// one `<% include!("..."); %>` token. Because the ranges are disjoint and
/// sorted, one forward scan splices every replacement in, avoiding the N scans
/// and N allocations of a sequential `String::replace` loop.
pub fn splice_include_replacements(
    contents: &str,
    mut replacements: Vec<(Range<usize>, String)>,
) -> String {
    replacements.sort_by_key(|(range, _)| range.start);

    let mut out = String::with_capacity(contents.len());
    let mut cursor = 0;
    for (range, replacement) in &replacements {
        debug_assert!(range.start >= cursor, "replacement ranges must be sorted and disjoint");
        out.push_str(&contents[cursor..range.start]);
        out.push_str(replacement);
        cursor = range.end;
    }
    out.push_str(&contents[cursor..]);
    out
}

/// Per-component processing outcome: the rewritten `<% include!(...) %>` text,
/// or the error from minifying the component.
type IncludeRewriteResult = Result<String, Box<dyn std::error::Error + Send + Sync>>;

#[allow(dead_code)]
fn minify_file_and_components_internal(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
    processed_files: Arc<Mutex<HashSet<PathBuf>>>,
) -> io::Result<()> {
    let canonical_path = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());

    // Claim the file for this template. Checking and inserting under one lock
    // means two parallel branches can never both minify (and write) it.
    if !processed_files.lock().unwrap().insert(canonical_path.clone()) {
        return Ok(());
    }

    {
        let global_cache = get_global_cache().lock().unwrap();
        if let Some(cached_output_path) = global_cache.get(&canonical_path) {
            if cached_output_path.as_path().exists() && new_path != cached_output_path.as_path() {
                create_dir_all(new_path.parent().unwrap()).expect("Cannot create dir");
                fs::copy(cached_output_path, new_path)?;
            }
            return Ok(());
        }
    }

    let mut input_file = File::open(file_path)?;
    let mut contents = String::new();
    input_file.read_to_string(&mut contents)?;

    let delimiter = minify_options.delimiter;
    let includes = extract_include_ranges(&contents, delimiter);

    if !includes.is_empty() {
        // A component included many times (an icon, a button) only needs to
        // be minified and rewritten once. `slot_of[i]` maps include `i` to its
        // file name's index in `unique_names`, kept in first-seen order.
        let mut unique_names: Vec<&str> = Vec::new();
        let mut slot_by_name: HashMap<&str, usize> = HashMap::new();
        let slot_of: Vec<usize> = includes
            .iter()
            .map(|(_, _, file_name)| {
                *slot_by_name.entry(file_name.as_str()).or_insert_with(|| {
                    unique_names.push(file_name.as_str());
                    unique_names.len() - 1
                })
            })
            .collect();

        let parent_output_dir = new_path.parent().unwrap().to_path_buf();
        let include_results: Vec<IncludeRewriteResult> =
            unique_names.par_iter().map(|&file_name| {
                let component_file_path = file_path.parent().unwrap().join(file_name);
                let child_new_path = modify_template_path(&component_file_path);

                minify_file_and_components_internal(
                    &component_file_path,
                    &child_new_path,
                    minify_options,
                    Arc::clone(&processed_files),
                )
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

                // Rewrite the include to a path relative to the minified parent.
                // Relative (rather than absolute) paths are required on Windows:
                // sailfish-compiler strips the first character of absolute include
                // paths, which mangles `C:\...` style paths. Sailfish parses the
                // argument as a Rust string literal, so it is written escaped
                // (`{:?}`): Windows separators would otherwise be read as
                // escapes (`sub\page.stpl.min`).
                let relative_path = pathdiff::diff_paths(&child_new_path, &parent_output_dir)
                    .unwrap_or_else(|| child_new_path.clone());
                let new_include = format!(
                    "<{delimiter} include!({:?}); {delimiter}>",
                    relative_path.to_string_lossy()
                );

                Ok(new_include)
            }).collect();

        let mut rewrites = Vec::with_capacity(include_results.len());
        for result in include_results {
            match result {
                Ok(new_include) => rewrites.push(new_include),
                Err(e) => {
                    eprintln!("Error processing component: {}", e);
                    return Err(io::Error::other("Component processing failed"));
                }
            }
        }

        // Pair every include's byte range with its file's rewrite. The ranges
        // are already in source order (extraction is ordered), so the splice
        // below needs a single pass.
        let replacements: Vec<(Range<usize>, String)> = includes
            .iter()
            .zip(&slot_of)
            .map(|((range, _, _), &slot)| (range.clone(), rewrites[slot].clone()))
            .collect();

        contents = splice_include_replacements(&contents, replacements);
    }

    create_dir_all(new_path.parent().unwrap())?;

    // Fast path for the in-process `Native` minifier: the minified output is
    // already in memory, so write it directly and skip the unminified
    // write → read → minified write disk roundtrip (saves two syscalls per
    // template, meaningful for deeply nested or many-include templates).
    let minified: Option<Vec<u8>> = {
        #[cfg(feature = "native-minifier")]
        {
            if matches!(minify_options.minifier, Minifier::Native) {
                // Keep the invocation counter in sync: `minify_file` is what
                // the unit tests use to observe cache behavior, and the fast
                // path performs the same minification without calling it.
                MINIFIER_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
                Some(minify_options.minify_native(&contents)?)
            } else {
                None
            }
        }
        #[cfg(not(feature = "native-minifier"))]
        {
            None
        }
    };

    if let Some(minified) = minified {
        fs::write(new_path, minified)?;
    } else {
        fs::write(new_path, contents)?;
        minify_options.minify_file(new_path, new_path)?;
    }

    {
        let mut global_cache = get_global_cache().lock().unwrap();
        global_cache.insert(canonical_path, new_path.to_path_buf());
    }

    Ok(())
}

/// Minify a template and all of its includes recursively. Only used when the
/// `minify-components` feature is enabled, but kept compiled in every
/// configuration because the unit tests in `sailfish-minify-core` exercise it
/// either way.
#[allow(dead_code)]
pub fn minify_file_and_components(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
) -> io::Result<()> {
    let processed_files = Arc::new(Mutex::new(HashSet::new()));
    minify_file_and_components_internal(file_path, new_path, minify_options, processed_files)
}

/// Copy a template and every template it transitively includes into `base`,
/// preserving the relative directory structure. Used when the
/// `minify-components` feature is disabled, so only the templates that are
/// actually referenced end up in the temp dir.
#[allow(dead_code)]
pub fn copy_referenced_template_and_includes(
    source: &Path,
    base: &Path,
    delimiter: char,
) -> io::Result<()> {
    let mut visited = HashSet::new();
    copy_referenced_template_and_includes_inner(source, base, delimiter, &mut visited)
}

#[allow(dead_code)]
fn copy_referenced_template_and_includes_inner(
    source: &Path,
    base: &Path,
    delimiter: char,
    visited: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let canonical = source.canonicalize().unwrap_or_else(|_| source.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }

    let destination = mirror_path(base, source);
    if let Some(parent) = destination.parent() {
        create_dir_all(parent)?;
    }
    fs::copy(source, &destination)?;

    let contents = fs::read_to_string(source)?;
    for (_, _, file_name) in extract_include_ranges(&contents, delimiter) {
        let child = source.parent().unwrap().join(file_name);
        copy_referenced_template_and_includes_inner(&child, base, delimiter, visited)?;
    }

    Ok(())
}

/// Minify a single template according to the enabled features.
///
/// * `minify-components` enabled: recursively minify the template and all of
///   its includes into the temp dir, rewriting include paths.
/// * disabled: copy only the referenced templates into the temp dir, then
///   minify the referenced file in place.
pub fn minify_template(
    file_path: &Path,
    new_path: &Path,
    minify_options: &MinifyOptions,
) -> io::Result<()> {
    #[cfg(feature = "minify-components")]
    {
        minify_file_and_components(file_path, new_path, minify_options)
    }

    #[cfg(not(feature = "minify-components"))]
    {
        copy_referenced_template_and_includes(file_path, tmp_main_path(), minify_options.delimiter)?;
        minify_options.minify_file(file_path, new_path)?;
        Ok(())
    }
}
