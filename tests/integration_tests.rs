use sailfish::TemplateSimple as RawTemplateSimple; // non-minified derive + trait for render_once()
use sailfish_minify::TemplateSimple as Minified; // minified derive

// ---------------------------------------------------------------------------
// Fixture structs
// ---------------------------------------------------------------------------

#[derive(Minified)]
#[template(path = "test/simple.stpl")]
struct SimpleTemplate {
    name: String,
}

/// Doc comments mentioning the path must not confuse template lookup.
#[derive(Minified)]
#[template(path = "test/simple.stpl")]
struct DocCommentTemplate {
    name: String,
}

#[cfg(feature = "native-minifier")]
#[derive(Minified)]
#[min_with(Native)]
#[template(path = "test/simple.stpl")]
struct NativeMinifiedTemplate {
    name: String,
}

#[derive(Minified)]
#[template(path = "test/small.stpl")]
struct SmallTemplate {
    name: String,
}

#[derive(RawTemplateSimple)]
#[template(path = "test/small.stpl")]
struct RawSmallTemplate {
    name: String,
}

#[derive(Minified)]
#[template(path = "test/with_includes.stpl")]
struct TemplateWithIncludes {
    items: Vec<String>,
}

#[derive(Minified)]
#[template(path = "test/nest_9.stpl")]
struct DeepNestedTemplate {
    _marker: bool,
}

#[derive(Minified)]
#[template(path = "test/many_includes.stpl")]
struct ManyIncludesTemplate {
    _marker: bool,
}

#[derive(Minified)]
#[template(path = "test/repeated_component.stpl")]
struct RepeatedComponentTemplate {
    items: Vec<String>,
}

#[derive(Minified)]
#[template(path = "test/empty.stpl")]
struct EmptyTemplate {
    _marker: bool,
}

struct Lang {
    code: String,
    name: String,
    sample_code: String,
}

#[derive(Minified)]
#[template(path = "test/unicode_heavy.stpl")]
struct UnicodeTemplate {
    langs: Vec<Lang>,
}

#[derive(Minified)]
#[min_with(Custom("html-minifier --collapse-whitespace"))]
#[template(path = "test/simple.stpl")]
struct CustomMinifiedTemplate {
    name: String,
}

#[derive(Minified)]
#[min_with(CustomUnchecked("html-minifier --remove-comments"))]
#[template(path = "test/simple.stpl")]
struct CustomUncheckedTemplate {
    name: String,
}

#[derive(Minified)]
#[template(path = "test/big_whitespace.stpl")]
struct BigWhitespaceTemplate {
    title: String,
}

#[derive(RawTemplateSimple)]
#[template(path = "test/big_whitespace.stpl")]
struct RawBigWhitespaceTemplate {
    title: String,
}

#[derive(Minified)]
#[template(path = "test/raw_html.stpl", escape = false)]
struct UnescapedTemplate {
    html: String,
}

#[derive(Minified)]
#[template(path = "test/custom_delimiter.stpl", delimiter = '#')]
struct CustomDelimiterTemplate {
    name: String,
}

#[derive(Minified)]
#[template(path = "test/subdir_includes.stpl")]
struct SubdirIncludesTemplate {
    _marker: bool,
}

#[derive(Minified)]
#[template(path = "test/sub/page.stpl")]
struct TemplateInSubdir {
    _marker: bool,
}

// `include!("sub\\part.stpl")` names `sub\part.stpl`, a nested file only on
// Windows; elsewhere sailfish itself would look for a file named that.
#[cfg(windows)]
#[derive(Minified)]
#[template(path = "test/backslash_include.stpl")]
struct BackslashIncludeTemplate {
    _marker: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_simple_template_renders_minified() {
    let result = SimpleTemplate { name: "World".into() }.render_once().unwrap();
    assert!(!result.starts_with('\n'));
    assert!(!result.contains("  "), "minified output should not contain double spaces");
}

#[test]
fn test_simple_template_contains_expected_content() {
    let result = SimpleTemplate { name: "World".into() }.render_once().unwrap();
    assert!(result.contains("World"));
}

#[test]
fn test_doc_comment_mentioning_path_compiles() {
    let result = DocCommentTemplate { name: "Doc".into() }.render_once().unwrap();
    assert!(result.contains("Hello, Doc"));
}

#[test]
fn test_escape_option_is_kept() {
    let result = UnescapedTemplate { html: "<b>bold</b>".into() }.render_once().unwrap();
    assert!(result.contains("<b>bold</b>"), "escape = false must survive the path rewrite: {result}");
}

#[test]
fn test_custom_delimiter_template() {
    let result = CustomDelimiterTemplate { name: "World".into() }.render_once().unwrap();
    // Exact whitespace around blocks differs between minifiers, so check the
    // parts: blocks evaluated, attribute quotes and the `<##` escape kept, and
    // the top-level template minified.
    assert!(result.starts_with(r#"<div title="World"><p>Hello, World</p><p><# is literal</p>"#), "{result}");
    assert!(result.contains("5"), "the included part should render: {result}");
    assert!(!result.contains("<#="), "every block should be evaluated: {result}");
}

#[test]
fn test_includes_across_subfolders() {
    // Covers a subfolder include, one nested two levels deep, a `../` include,
    // and the same component reached through two different relative paths.
    let result = SubdirIncludesTemplate { _marker: true }.render_once().unwrap();
    let positions: Vec<usize> = ["part", "leaf", "shared"]
        .iter()
        .map(|text| result.find(text).unwrap_or_else(|| panic!("missing {text:?}: {result}")))
        .collect();
    assert!(positions.is_sorted(), "includes should render in order: {result}");
    assert_eq!(result.matches("<footer>").count(), 2, "{result}");
}

#[test]
fn test_template_in_subfolder_with_parent_include() {
    let result = TemplateInSubdir { _marker: true }.render_once().unwrap();
    assert!(result.contains("part") && result.contains("leaf"), "{result}");
    assert!(result.contains("shared"), "the `../` include should render: {result}");
}

#[cfg(windows)]
#[test]
fn test_backslash_include_path() {
    let result = BackslashIncludeTemplate { _marker: true }.render_once().unwrap();
    assert!(result.contains("part"), "{result}");
}

#[test]
fn test_minified_output_smaller_than_unminified() {
    let minified = SmallTemplate { name: "World".into() }.render_once().unwrap();
    let raw = RawSmallTemplate { name: "World".into() }.render_once().unwrap();
    assert!(minified.len() <= raw.len());
    assert!(minified.contains("World"));
}

#[test]
fn test_includes_are_minified_recursively() {
    let result = TemplateWithIncludes {
        items: vec!["a".into(), "b".into()],
    }
    .render_once()
    .unwrap();
    #[cfg(feature = "minify-components")]
    assert!(!result.contains("  "), "child includes should have no extra whitespace");
    assert!(result.contains("<main>"));
    assert!(result.contains("<p>a</p>"));
    assert!(result.contains("<p>b</p>"));
}

#[test]
fn test_deeply_nested_includes() {
    let result = DeepNestedTemplate { _marker: true }.render_once().unwrap();
    assert!(result.contains("deepest"), "leaf of the 10-level include chain should render");
    assert!(result.contains("Level 9"));
}

#[test]
fn test_many_parallel_includes() {
    let result = ManyIncludesTemplate { _marker: true }.render_once().unwrap();
    for i in 0..50 {
        assert!(result.contains(&format!("Sibling {:02}", i)));
    }
    assert!(!result.contains("  "));
}

#[test]
fn test_repeated_component_included_many_times() {
    let items: Vec<String> = (0..50).map(|i| format!("item-{}", i)).collect();
    let result = RepeatedComponentTemplate { items }.render_once().unwrap();
    assert_eq!(result.matches("shared-component").count(), 50);
    #[cfg(feature = "minify-components")]
    assert!(!result.contains("  "));
}

#[test]
fn test_empty_template() {
    let result = EmptyTemplate { _marker: true }.render_once().unwrap();
    assert!(result.trim().is_empty(), "empty template should not crash and render empty output");
}

#[test]
fn test_unicode_content_preserved() {
    let langs = vec![
        Lang {
            code: "ja".into(),
            name: "Japanese".into(),
            sample_code: "fn main() {}".into(),
        },
        Lang {
            code: "ar".into(),
            name: "Arabic".into(),
            sample_code: "print('مرحبا')".into(),
        },
        Lang {
            code: "he".into(),
            name: "Hebrew".into(),
            sample_code: "print('שלום')".into(),
        },
    ];
    let result = UnicodeTemplate { langs }.render_once().unwrap();
    assert!(result.contains("こんにちは"));
    assert!(result.contains("你好，世界"));
    assert!(result.contains("مرحباً بالعالم"));
    assert!(result.contains("שלום עולם"));
    assert!(result.contains("🚀"));
    assert!(result.contains("∀x ∈ ℝ"));
}

#[test]
fn test_custom_minifier() {
    let result = CustomMinifiedTemplate { name: "World".into() }.render_once().unwrap();
    assert!(!result.contains("  "));
    assert!(result.contains("World"));
}

#[cfg(feature = "native-minifier")]
#[test]
fn test_native_minifier_renders_minified() {
    let result = NativeMinifiedTemplate { name: "World".into() }.render_once().unwrap();
    assert!(!result.contains("  "), "native minified output should not contain double spaces");
    assert!(result.contains("World"));
    assert!(result.contains("<h1>Hello,"), "interpolated content should survive native minification");
}

#[test]
fn test_different_minifier_options_per_template() {
    let a = CustomMinifiedTemplate { name: "A".into() }.render_once().unwrap();
    let b = CustomUncheckedTemplate { name: "B".into() }.render_once().unwrap();
    assert!(a.contains("A"));
    assert!(b.contains("B"));
    assert!(!a.contains("  "));
    assert!(!b.contains("  "));
}

// ---------------------------------------------------------------------------
// Big whitespace-heavy template
// ---------------------------------------------------------------------------

fn whitespace_ratio(s: &str) -> f64 {
    let ws = s.chars().filter(|c| c.is_whitespace()).count();
    ws as f64 / s.len() as f64
}

#[test]
fn test_big_whitespace_template_is_large() {
    let raw = RawBigWhitespaceTemplate { title: "Big".into() }.render_once().unwrap();
    assert!(
        raw.len() > 1_000_000,
        "raw big template should be a very big file (>= 1MB), got {} bytes",
        raw.len()
    );
    assert!(
        whitespace_ratio(&raw) > 0.3,
        "fixture should contain a lot of whitespace"
    );
}

#[test]
fn test_big_whitespace_template_minifies_aggressively() {
    let raw = RawBigWhitespaceTemplate { title: "Big".into() }.render_once().unwrap();
    let minified = BigWhitespaceTemplate { title: "Big".into() }.render_once().unwrap();

    assert!(
        minified.len() < raw.len(),
        "minified ({} bytes) should be smaller than raw ({} bytes)",
        minified.len(),
        raw.len()
    );
    let raw_ws = whitespace_ratio(&raw);
    let min_ws = whitespace_ratio(&minified);
    assert!(
        min_ws < raw_ws * 0.5,
        "whitespace ratio should collapse (raw {:.3}, minified {:.3})",
        raw_ws,
        min_ws
    );
}

#[test]
fn test_big_whitespace_template_content_preserved() {
    let minified = BigWhitespaceTemplate { title: "Big".into() }.render_once().unwrap();
    assert!(minified.contains("Big"), "interpolated value should survive");
    assert!(
        minified.contains("Section Heading 500"),
        "body content should survive minification"
    );
    assert!(
        minified.contains("UNIQUE_MARKER_FOOTER"),
        "footer marker should survive minification"
    );
    assert!(!minified.contains("  "), "minified output should have no double spaces");
    assert!(!minified.starts_with('\n'));
}
