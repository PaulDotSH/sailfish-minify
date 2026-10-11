use sailfish_minify_core::{
    extract_includes, extract_template_path, get_minify_options_from_token_stream,
    minify_file_and_components, parse_derive_input, replace_path_attribute, tmp_main_path,
    Minifier, MinifyOptions, MINIFIER_INVOCATIONS,
};
#[cfg(feature = "regex")]
use sailfish_minify_core::get_include_regex;
use sailfish_minify_core::extract_includes_manual;
#[cfg(not(feature = "minify-components"))]
use sailfish_minify_core::copy_referenced_template_and_includes;
#[cfg(feature = "minify-components")]
use sailfish_minify_core::modify_template_path;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use tempfile::{tempdir, tempdir_in};

/// The cache tests shell out to the minifier and read the shared
/// `MINIFIER_INVOCATIONS` counter, so they must not run concurrently.
static MINIFY_LOCK: Mutex<()> = Mutex::new(());

fn with_minify_lock<F: FnOnce()>(f: F) {
    let _guard = MINIFY_LOCK.lock().unwrap();
    MINIFIER_INVOCATIONS.store(0, Ordering::SeqCst);
    f();
}

// ---------------------------------------------------------------------------
// Template Path Extraction
// ---------------------------------------------------------------------------

#[test]
fn test_extract_template_path_simple() {
    let path = extract_template_path(r#"#[template(path = "foo.stpl")] struct Foo;"#).unwrap();
    assert_eq!(path, Path::new("./templates").join("foo.stpl"));
}

#[test]
fn test_extract_template_path_nested() {
    let path = extract_template_path(r#"#[template(path = "sub/bar.stpl")] struct Foo;"#).unwrap();
    assert_eq!(path, Path::new("./templates").join("sub/bar.stpl"));
}

#[test]
fn test_extract_template_path_missing_panics() {
    let result = extract_template_path("struct Foo { name: String }");
    assert!(result.is_err(), "missing #[template] attribute should be an error, not a panic");
}

#[test]
fn test_extract_template_path_ignores_doc_comments() {
    // Doc comments become `#[doc = "..."]` attributes ahead of `#[template]`,
    // so a doc mentioning "path" must not be mistaken for the attribute.
    let path = extract_template_path(
        r#"/// Renders the path breadcrumbs.
        #[template(path = "foo.stpl")] struct Foo;"#,
    )
    .unwrap();
    assert_eq!(path, Path::new("./templates").join("foo.stpl"));
}

#[test]
fn test_parse_derive_input_reads_all_attributes() {
    let tokens: proc_macro2::TokenStream = r#"
        /// Uses a custom path.
        #[derive(Debug)]
        #[min_with(Custom("my-minifier"))]
        #[template(rm_whitespace = true, path = "sub/bar.stpl")]
        struct Foo { path: String }
    "#
    .parse()
    .unwrap();
    let input = parse_derive_input(tokens).unwrap();
    assert_eq!(input.template_path, Path::new("./templates").join("sub/bar.stpl"));
    assert_eq!(input.minify_options.minifier, Minifier::Custom("my-minifier".to_string()));
}

// ---------------------------------------------------------------------------
// Path Attribute Replacement
// ---------------------------------------------------------------------------

fn template_attr_path(item: &syn::ItemStruct) -> String {
    let attr = item
        .attrs
        .iter()
        .find(|a| a.path().is_ident("template"))
        .expect("template attribute should be present");
    let meta = attr.parse_args::<syn::Meta>().unwrap();
    match meta {
        syn::Meta::NameValue(nv) => match nv.value {
            syn::Expr::Lit(lit) => match lit.lit {
                syn::Lit::Str(s) => s.value(),
                _ => panic!("template path value should be a string literal"),
            },
            _ => panic!("template path value should be a string literal"),
        },
        _ => panic!("template attribute should be a name-value pair"),
    }
}

#[test]
fn test_replace_path_attribute_rewrites_correctly() {
    let input: proc_macro2::TokenStream =
        r#"#[template(path = "foo.stpl")] struct Foo { name: String }"#.parse().unwrap();
    let output = replace_path_attribute(input, "C:\\tmp\\foo.stpl.min");

    let item: syn::ItemStruct = syn::parse2(output).unwrap();
    assert_eq!(template_attr_path(&item), "C:\\tmp\\foo.stpl.min");
}

#[test]
fn test_replace_path_attribute_preserves_other_attrs() {
    let input: proc_macro2::TokenStream =
        r#"#[derive(Debug)] #[template(path = "foo.stpl")] struct Foo { name: String }"#
            .parse()
            .unwrap();
    let output = replace_path_attribute(input, "out.stpl.min");

    let item: syn::ItemStruct = syn::parse2(output).unwrap();
    assert!(item.attrs.iter().any(|a| a.path().is_ident("derive")), "derive attribute should be preserved");
    assert!(item.attrs.iter().any(|a| a.path().is_ident("template")), "template attribute should be present");
    assert_eq!(template_attr_path(&item), "out.stpl.min");
}

// ---------------------------------------------------------------------------
// Include Regex Parsing
// ---------------------------------------------------------------------------

#[cfg(feature = "regex")]
#[test]
fn test_include_regex_simple() {
    let contents = r#"<div><% include!("header.stpl"); %></div>"#;
    let matches: Vec<String> = get_include_regex()
        .captures_iter(contents)
        .map(|cap| cap[1].to_string())
        .collect();
    assert_eq!(matches, vec!["header.stpl"]);
}

#[test]
fn test_include_regex_with_whitespace() {
    let contents = r#"<%   include! (  "file.stpl" )  ;  %>"#;
    assert_eq!(extract_includes(contents), vec!["file.stpl"]);
}

#[test]
fn test_include_regex_no_match_on_normal_template_code() {
    let contents = r#"<% for item in items { %><div><%= item %></div><% } %>"#;
    assert!(extract_includes(contents).is_empty());
}

#[test]
fn test_include_regex_multiple_matches() {
    let contents = concat!(
        r#"<% include!("a.stpl"); %>"#,
        r#"<% include!("b.stpl"); %>"#,
        r#"<% include!("c.stpl"); %>"#,
        r#"<% include!("d.stpl"); %>"#,
        r#"<% include!("e.stpl"); %>"#,
    );
    assert_eq!(
        extract_includes(contents),
        vec!["a.stpl", "b.stpl", "c.stpl", "d.stpl", "e.stpl"]
    );
}

#[test]
fn test_include_manual_parser_matches_regex() {
    // The manual parser must agree with the regex on every case the regex
    // allows, including `\s*` (tabs/newlines) between tokens. Note the regex
    // only permits spaces between `<%` and `include!`.
    let contents = concat!(
        "<main>\n",
        "  <% include!\t(\n \"tab.stpl\"\n )\n;\n %>\n",
        "  <p>text</p>\n",
        "<% include!(\"with space.stpl\"); %>\n",
        "  <% let x = 1; %>\n",
        "<%= include!(\"not-a-path\") %>\n",
    );
    let manual: Vec<String> = extract_includes_manual(contents)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(manual, vec!["tab.stpl", "with space.stpl"]);
}

#[test]
fn test_include_manual_parser_spans_are_exact() {
    // The full-match spans must round-trip exactly so `contents.replace`
    // targets the same text the regex would have matched.
    let contents = r#"<div><% include!("header.stpl"); %></div>"#;
    let spans = extract_includes_manual(contents);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].0, r#"<% include!("header.stpl"); %>"#);
    assert_eq!(spans[0].1, "header.stpl");
    assert!(contents.contains(&spans[0].0));
}

// ---------------------------------------------------------------------------
// Minify Options Parsing
// ---------------------------------------------------------------------------

#[cfg(feature = "native-minifier")]
#[test]
fn test_default_minifier_is_native() {
    let options = MinifyOptions::default();
    assert_eq!(options.minifier, Minifier::Native);
}

#[cfg(not(feature = "native-minifier"))]
#[test]
fn test_default_minifier_is_html_minifier() {
    let options = MinifyOptions::default();
    assert_eq!(options.minifier, Minifier::HTMLMinifier);
}

#[test]
fn test_native_minifier_parsed() {
    let tokens: proc_macro2::TokenStream =
        r#"#[min_with(Native)] struct Foo;"#.parse().unwrap();
    let mut options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens, &mut options).unwrap();
    assert_eq!(options.minifier, Minifier::Native);
}

#[cfg(feature = "native-minifier")]
#[test]
fn test_native_minifier_minifies_file() {
    with_minify_lock(|| {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.html");
        let output = dir.path().join("out.html");
        fs::write(&input, "<div>\n  <p>  Hello   world  </p>\n</div>").unwrap();

        let options = MinifyOptions { minifier: Minifier::Native };
        options.minify_file(&input, &output).unwrap();

        let minified = fs::read_to_string(&output).unwrap();
        assert_eq!(minified, "<div><p>Hello world</p></div>");
    });
}

#[test]
fn test_custom_minifier() {
    let tokens: proc_macro2::TokenStream =
        r#"#[min_with(Custom("my-minifier --fast"))] struct Foo;"#.parse().unwrap();
    let mut options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens, &mut options).unwrap();
    assert_eq!(
        options.minifier,
        Minifier::Custom("my-minifier --fast".to_string())
    );
}

#[test]
fn test_custom_unchecked_minifier() {
    let tokens: proc_macro2::TokenStream =
        r#"#[min_with(CustomUnchecked("my-minifier"))] struct Foo;"#.parse().unwrap();
    let mut options = MinifyOptions::default();
    get_minify_options_from_token_stream(tokens, &mut options).unwrap();
    assert_eq!(
        options.minifier,
        Minifier::CustomUnchecked("my-minifier".to_string())
    );
}

#[test]
fn test_invalid_minifier_panics() {
    let tokens: proc_macro2::TokenStream =
        r#"#[min_with(Unknown)] struct Foo;"#.parse().unwrap();
    let mut options = MinifyOptions::default();
    assert!(
        get_minify_options_from_token_stream(tokens, &mut options).is_err(),
        "unknown minifier should be an error, not a panic"
    );
}

// ---------------------------------------------------------------------------
// Cache Behavior
// ---------------------------------------------------------------------------

#[test]
fn test_cache_hit_skips_minification() {
    with_minify_lock(|| {
        let dir = tempdir().unwrap();
        let src = dir.path().join("tpl.stpl");
        fs::write(&src, "<html><body><h1>Cache</h1></body></html>").unwrap();
        let out = dir.path().join("tpl.stpl.min");

        minify_file_and_components(&src, &out, &MinifyOptions::default()).unwrap();
        assert_eq!(MINIFIER_INVOCATIONS.load(Ordering::SeqCst), 1);

        minify_file_and_components(&src, &out, &MinifyOptions::default()).unwrap();
        assert_eq!(
            MINIFIER_INVOCATIONS.load(Ordering::SeqCst),
            1,
            "second compile of the same template should hit the cache and skip minification"
        );
    });
}

#[test]
fn test_cache_key_is_canonical_path() {
    with_minify_lock(|| {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempdir_in(&cwd).unwrap();
        let abs = dir.path().join("tpl.stpl");
        fs::write(&abs, "<html><body><h1>Canonical</h1></body></html>").unwrap();
        let rel = abs.strip_prefix(&cwd).unwrap();
        let out = dir.path().join("tpl.stpl.min");

        minify_file_and_components(rel, &out, &MinifyOptions::default()).unwrap();
        assert_eq!(MINIFIER_INVOCATIONS.load(Ordering::SeqCst), 1);

        minify_file_and_components(&abs, &out, &MinifyOptions::default()).unwrap();
        assert_eq!(
            MINIFIER_INVOCATIONS.load(Ordering::SeqCst),
            1,
            "relative and absolute paths to the same file should share a cache entry"
        );
    });
}

#[test]
fn test_repeated_include_minified_once() {
    with_minify_lock(|| {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("icon.stpl"), "<i> x </i>").unwrap();
        let main = dir.path().join("main.stpl");
        fs::write(&main, r#"<p><% include!("icon.stpl"); %></p>"#.repeat(20)).unwrap();
        let out = dir.path().join("main.stpl.min");

        minify_file_and_components(&main, &out, &MinifyOptions::default()).unwrap();
        assert_eq!(
            MINIFIER_INVOCATIONS.load(Ordering::SeqCst),
            2,
            "a component included many times should be minified once, plus the parent"
        );

        let minified = fs::read_to_string(&out).unwrap();
        assert_eq!(minified.matches("include!").count(), 20, "every include should be kept");
        assert!(
            !minified.contains(r#"include!("icon.stpl")"#),
            "every include should be rewritten to the minified component"
        );
    });
}

#[test]
fn test_component_shared_across_templates() {
    with_minify_lock(|| {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("component.stpl"), "<div>shared</div>").unwrap();
        fs::write(
            dir.path().join("a.stpl"),
            r#"<main><% include!("component.stpl"); %></main>"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("b.stpl"),
            r#"<footer><% include!("component.stpl"); %></footer>"#,
        )
        .unwrap();

        minify_file_and_components(
            &dir.path().join("a.stpl"),
            &dir.path().join("a.min"),
            &MinifyOptions::default(),
        )
        .unwrap();
        minify_file_and_components(
            &dir.path().join("b.stpl"),
            &dir.path().join("b.min"),
            &MinifyOptions::default(),
        )
        .unwrap();

        // a.stpl + component.stpl + b.stpl = 3 minifications.
        // component.stpl is shared and must only be minified once.
        assert_eq!(
            MINIFIER_INVOCATIONS.load(Ordering::SeqCst),
            3,
            "shared component should be minified exactly once"
        );
    });
}

// ---------------------------------------------------------------------------
// Cross-Platform Temp Path
// ---------------------------------------------------------------------------

#[test]
fn test_temp_path_uses_system_temp_dir() {
    let path = tmp_main_path();
    assert!(path.starts_with(std::env::temp_dir()));
    assert_eq!(
        path.file_name().map(|n| n.to_string_lossy().into_owned()),
        Some("sailfish-minify".to_string())
    );
}

#[test]
fn test_temp_path_creatable_on_windows() {
    let path = tmp_main_path().join("test-writable");
    fs::create_dir_all(&path).unwrap();
    let probe = path.join("probe.txt");
    fs::write(&probe, "ok").unwrap();
    assert!(probe.exists());
    fs::remove_dir_all(&path).unwrap();
}

// ---------------------------------------------------------------------------
// On-Demand Copy (No Minify-Components)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "minify-components"))]
fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            total += count_files(&entry.path());
        } else {
            total += 1;
        }
    }
    total
}

#[cfg(not(feature = "minify-components"))]
#[test]
fn test_only_referenced_template_copied() {
    let cwd = std::env::current_dir().unwrap();
    let dir = tempdir_in(&cwd).unwrap();
    let templates = dir.path().join("templates");
    fs::create_dir_all(&templates).unwrap();
    for i in 0..100 {
        fs::write(
            templates.join(format!("tpl_{}.stpl", i)),
            format!("<div>template {}</div>", i),
        )
        .unwrap();
    }

    let referenced_abs = templates.join("tpl_0.stpl");
    let referenced = referenced_abs.strip_prefix(&cwd).unwrap().to_path_buf();
    let base = tempdir().unwrap();

    copy_referenced_template_and_includes(&referenced, base.path()).unwrap();

    assert_eq!(
        count_files(base.path()),
        1,
        "only the referenced template should be copied to the temp dir"
    );
    let expected_dest: std::path::PathBuf = base.path().join(&referenced);
    assert!(expected_dest.exists());
}

#[cfg(feature = "minify-components")]
#[test]
fn test_modify_template_path_places_file_in_tmp() {
    let source = Path::new("./templates/test/simple.stpl");
    let new_path = modify_template_path(source);
    assert!(new_path.starts_with(tmp_main_path()));
    assert!(new_path.to_string_lossy().ends_with("simple.stpl.min"));
}
