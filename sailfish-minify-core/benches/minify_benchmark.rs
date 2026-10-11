use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use quote::ToTokens;
use sailfish_minify_core::{
    extract_includes_manual, minify_file_and_components, modify_template_path,
    parse_derive_input, set_template_path, splice_include_replacements, MinifyOptions,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::LazyLock;

fn generate_html_of_size(size: usize) -> String {
    let unit = "<div class=\"row\">\n  <p>Lorem ipsum dolor sit amet consectetur adipiscing elit.</p>\n  <span>Item</span>\n</div>\n";
    let mut s = String::with_capacity(size);
    s.push_str("<!DOCTYPE html><html><head><title>Bench</title></head><body>\n");
    let mut remaining = size;
    while remaining > unit.len() {
        s.push_str(unit);
        remaining -= unit.len();
    }
    for _ in 0..remaining {
        s.push(' ');
    }
    s.push_str("\n</body></html>");
    s
}

fn minify_via_cli(html: &str) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.html");
    let output = dir.path().join("out.html");
    std::fs::write(&input, html).unwrap();
    let result = Command::new(sailfish_minify_core::resolve_command("html-minifier"))
        .args(["--collapse-whitespace", input.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(result.status.success());
}

#[cfg(feature = "native-minifier")]
fn minify_via_native(html: &str) {
    // Pure-Rust minification via minify-html: in-memory, no process spawn.
    let minified =
        minify_html::minify(html.as_bytes(), &sailfish_minify_core::native_minify_cfg());
    black_box(minified);
}

#[cfg(not(feature = "native-minifier"))]
fn minify_via_native_placeholder(html: &str) {
    // Fallback when the `native-minifier` feature is disabled: no-op passthrough.
    black_box(html.as_bytes().to_vec());
}

fn compile_with_include_depth(depth: usize) {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..depth {
        let content = if i == 0 {
            "<div>leaf</div>".to_string()
        } else {
            format!("<section><% include!(\"n{}.stpl\"); %></section>", i - 1)
        };
        std::fs::write(dir.path().join(format!("n{}.stpl", i)), content).unwrap();
    }
    std::fs::write(
        dir.path().join("main.stpl"),
        format!("<main><% include!(\"n{}.stpl\"); %></main>", depth - 1),
    )
    .unwrap();
    minify_file_and_components(
        &dir.path().join("main.stpl"),
        &dir.path().join("out.min"),
        &MinifyOptions::default(),
    )
    .unwrap();
}

fn compile_with_n_includes(count: usize) {
    let dir = tempfile::tempdir().unwrap();
    let mut main = String::from("<main>");
    for i in 0..count {
        std::fs::write(
            dir.path().join(format!("s{}.stpl", i)),
            format!("<div>sibling {}</div>", i),
        )
        .unwrap();
        main.push_str(&format!("<% include!(\"s{}.stpl\"); %>", i));
    }
    main.push_str("</main>");
    std::fs::write(dir.path().join("main.stpl"), &main).unwrap();
    minify_file_and_components(
        &dir.path().join("main.stpl"),
        &dir.path().join("out.min"),
        &MinifyOptions::default(),
    )
    .unwrap();
}

static FRESH_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn compile_fresh_template() {
    let dir = tempfile::tempdir().unwrap();
    let n = FRESH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = dir.path().join(format!("fresh{}.stpl", n));
    std::fs::write(&path, "<html><body><h1>fresh</h1></body></html>").unwrap();
    minify_file_and_components(
        &path,
        &dir.path().join(format!("out{}.min", n)),
        &MinifyOptions::default(),
    )
    .unwrap();
}

static CACHED_TEMPLATE: LazyLock<PathBuf> = LazyLock::new(|| {
    let base = std::env::temp_dir().join("sailfish-minify-bench-cache");
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("cached.stpl");
    std::fs::write(&path, "<html><body><h1>cached</h1></body></html>").unwrap();
    path
});

fn compile_cached_template() {
    let path = CACHED_TEMPLATE.clone();
    minify_file_and_components(
        &path,
        &path.with_extension("cached.min"),
        &MinifyOptions::default(),
    )
    .unwrap();
}

fn bench_template_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("template_size");
    let sizes = [0, 1024, 102_400, 1_048_576, 10_485_760]; // 0, 1KB, 100KB, 1MB, 10MB

    for size in sizes.iter() {
        let html = generate_html_of_size(*size);
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::new("minify-html-cli", size), &html, |b, html| {
            b.iter(|| minify_via_cli(black_box(html)))
        });
        #[cfg(feature = "native-minifier")]
        group.bench_with_input(BenchmarkId::new("minify-html-native", size), &html, |b, html| {
            b.iter(|| minify_via_native(black_box(html)))
        });
        #[cfg(not(feature = "native-minifier"))]
        group.bench_with_input(
            BenchmarkId::new("native-disabled-noop", size),
            &html,
            |b, html| b.iter(|| minify_via_native_placeholder(black_box(html))),
        );
    }
    group.finish();
}

fn bench_include_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("include_depth");
    for depth in [1, 5, 10, 20, 50].iter() {
        group.bench_with_input(BenchmarkId::new("depth", depth), depth, |b, &depth| {
            b.iter(|| compile_with_include_depth(black_box(depth)))
        });
    }
    group.finish();
}

fn bench_parallel_includes(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_includes");
    for count in [1, 10, 50, 100, 500].iter() {
        group.bench_with_input(BenchmarkId::new("count", count), count, |b, &count| {
            b.iter(|| compile_with_n_includes(black_box(count)))
        });
    }
    group.finish();
}

fn bench_cache_hit_vs_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache");
    group.bench_function("cache_miss_first_compile", |b| {
        b.iter(compile_fresh_template)
    });
    group.bench_function("cache_hit_second_compile", |b| {
        compile_fresh_template(); // prime cache
        b.iter(compile_cached_template)
    });
    group.finish();
}

fn generate_template_with_includes(count: usize) -> String {
    let mut s = String::from("<main>\n");
    for i in 0..count {
        s.push_str(&format!(
            "<section class=\"s\">\n  <p>text {}</p>\n  <% include!(\"comp{}.stpl\"); %>\n</section>\n",
            i, i
        ));
    }
    s.push_str("</main>\n");
    s
}

fn bench_include_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("include_parse");
    for count in [0, 1, 10, 100, 1000].iter() {
        let template = generate_template_with_includes(*count);
        group.throughput(Throughput::Bytes(template.len() as u64));
        group.bench_with_input(BenchmarkId::new("manual", count), &template, |b, t| {
            b.iter(|| black_box(extract_includes_manual(black_box(t)).len()))
        });
        #[cfg(feature = "regex")]
        group.bench_with_input(BenchmarkId::new("regex", count), &template, |b, t| {
            b.iter(|| black_box(sailfish_minify_core::extract_includes(black_box(t)).len()))
        });
    }
    group.finish();
}

/// A single repeating HTML chunk carrying 3 includes (~220 bytes each). A
/// large file is just many of these, giving a dense include workload that
/// stresses raw scan throughput rather than per-include call overhead.
const LARGE_CHUNK: &str = "<section class=\"item\">\n  <h2>Heading text</h2>\n  <p>Some body copy to pad the file out to a realistic size.</p>\n  <% include!(\"a.stpl\"); %>\n  <% include!(\"b.stpl\"); %>\n  <% include!(\"c.stpl\"); %>\n</section>\n";

fn generate_large_template_with_includes(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes);
    while s.len() < target_bytes {
        s.push_str(LARGE_CHUNK);
    }
    s
}

fn bench_include_parse_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("include_parse_large");
    for size in [1_048_576, 5_242_880, 10_485_760].iter() {
        // 1 MiB / 5 MiB / 10 MiB, each with ~3 includes per ~220-byte chunk.
        let template = generate_large_template_with_includes(*size);
        let include_count = extract_includes_manual(&template).len();
        group.throughput(Throughput::Bytes(template.len() as u64));
        group.bench_with_input(BenchmarkId::new("manual", size), &template, |b, t| {
            b.iter(|| black_box(extract_includes_manual(black_box(t)).len()))
        });
        #[cfg(feature = "regex")]
        group.bench_with_input(BenchmarkId::new("regex", size), &template, |b, t| {
            b.iter(|| black_box(sailfish_minify_core::extract_includes(black_box(t)).len()))
        });
        println!("include_parse_large: {} MiB input -> {} include matches", size / 1024 / 1024, include_count);
    }
    group.finish();
}

/// A large template of `target_bytes` whose body repeats `count` distinct
/// `<% include!(...) %>` tokens cyclically. Each distinct token recurs many
/// times, so a sequential `.replace()` loop re-scans the whole file once per
/// distinct token, which is the exact cost profile the single-pass splice
/// eliminates.
fn generate_replace_workload(target_bytes: usize, count: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 256);
    while s.len() < target_bytes {
        for i in 0..count {
            s.push_str(&format!("<% include!(\"comp{}.stpl\"); %>", i));
            s.push_str("<p>Lorem ipsum dolor sit amet consectetur.</p>\n");
        }
    }
    s
}

/// Benchmarks the two ways of applying include-path rewrites to a template:
/// the old sequential `String::replace` loop (N scans + N allocations) versus
/// the new single-pass splice (1 scan + 1 allocation).
fn bench_include_replace(c: &mut Criterion) {
    let mut group = c.benchmark_group("include_replace");
    for (size, count) in [(1 << 16, 10), (1 << 18, 50), (1 << 18, 200)] {
        // 64 KiB body with 10 tokens; 256 KiB bodies with 50 / 200 tokens.
        let template = generate_replace_workload(size, count);

        // Needle -> replacement pairs, exactly as the sequential loop consumed
        // them (each distinct token appears many times in the body).
        let needles: Vec<(String, String)> = extract_includes_manual(&template)
            .into_iter()
            .map(|(full, name)| {
                let rewritten = format!(r#"<% include!("rewritten/{}.min"); %>"#, name);
                (full, rewritten)
            })
            .collect();

        // Position-based replacements, exactly as the single-pass splice
        // consumes them.
        let ranges: Vec<(std::ops::Range<usize>, String)> =
            sailfish_minify_core::extract_include_ranges(&template, '%')
                .into_iter()
                .map(|(range, _full, name)| {
                    (range, format!(r#"<% include!("rewritten/{}.min"); %>"#, name))
                })
                .collect();
        assert_eq!(needles.len(), ranges.len(), "both approaches must see the same replacements");

        // Both approaches must produce byte-identical output on this workload.
        let sequential = {
            let mut contents = template.clone();
            for (original, rewritten) in &needles {
                contents = contents.replace(original, rewritten);
            }
            contents
        };
        let spliced = splice_include_replacements(&template, ranges.clone());
        assert_eq!(sequential, spliced, "single-pass splice must match sequential replace");

        group.throughput(Throughput::Bytes(template.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("sequential-replace", count),
            &(&template, &needles),
            |b, (template, needles)| {
                b.iter(|| {
                    let mut contents = template.to_string();
                    for (original, rewritten) in needles.iter() {
                        contents = contents.replace(original.as_str(), rewritten.as_str());
                    }
                    black_box(contents)
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("single-pass-splice", count),
            &(&template, &ranges),
            |b, (template, ranges)| {
                b.iter(|| {
                    let replacements = ranges.to_vec();
                    black_box(splice_include_replacements(template.as_str(), replacements))
                })
            },
        );
    }
    group.finish();
}

/// A `#[derive(TemplateSimple)]` input with `fields` documented fields, as the
/// proc macro receives it.
fn derive_input_tokens(fields: usize) -> proc_macro2::TokenStream {
    let mut s = String::from(
        "/// A page rendered by sailfish.
#[derive(Debug, Clone)]
#[min_with(Native)]
#[template(path = \"pages/index.stpl\")]
pub struct Page {
",
    );
    for i in 0..fields {
        s.push_str(&format!("    /// Field number {i}.
    pub field_{i}: Vec<Option<String>>,
"));
    }
    s.push('}');
    s.parse().unwrap()
}

/// Everything `derive_template_simple` does to its input before minifying and
/// handing it to sailfish: find the template path, read `#[min_with]`, and
/// rewrite the path attribute.
fn process_derive_input(tokens: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let mut input = parse_derive_input(tokens).unwrap();
    let new_path = modify_template_path(&input.template_path);
    black_box(&input.minify_options);
    set_template_path(&mut input.item, new_path.to_str().unwrap());
    input.item.into_token_stream()
}

fn bench_derive_input(c: &mut Criterion) {
    let mut group = c.benchmark_group("derive_input");
    for fields in [1, 10, 50] {
        let tokens = derive_input_tokens(fields);
        group.bench_with_input(BenchmarkId::new("fields", fields), &tokens, |b, tokens| {
            b.iter_batched(|| tokens.clone(), process_derive_input, BatchSize::SmallInput)
        });
    }
    group.finish();
}

/// A template including the same component `count` times, e.g. an icon or a
/// button used throughout a page. Only the minification is timed; the temp dir
/// setup and teardown are excluded.
fn bench_duplicate_includes(c: &mut Criterion) {
    let mut group = c.benchmark_group("duplicate_includes");
    for count in [1, 10, 50, 200] {
        group.bench_with_input(BenchmarkId::new("count", count), &count, |b, &count| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    std::fs::write(dir.path().join("icon.stpl"), "<i class=\"icon\"> x </i>")
                        .unwrap();
                    let main = "<span><% include!(\"icon.stpl\"); %></span>
".repeat(count);
                    std::fs::write(dir.path().join("main.stpl"), main).unwrap();
                    dir
                },
                |dir| {
                    minify_file_and_components(
                        &dir.path().join("main.stpl"),
                        &dir.path().join("out.min"),
                        &MinifyOptions::default(),
                    )
                    .unwrap();
                    dir
                },
                BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

/// A ~100 KiB template dense with sailfish blocks, written with `delimiter`.
fn generate_template_with_blocks(delimiter: char) -> String {
    let chunk = "<li class=\"item\">\n  <% if item.active { %>\n    <a href=\"<%= item.url %>\">  <%= item.name %>  </a>\n  <% } %>\n</li>\n";
    chunk
        .repeat(100 * 1024 / chunk.len())
        .replace("<%", &format!("<{delimiter}"))
        .replace("%>", &format!("{delimiter}>"))
}

/// Native minification of the same template with the default `%` delimiter and
/// a custom `#` one, which first swaps every block for a placeholder.
#[cfg(feature = "native-minifier")]
fn bench_custom_delimiter(c: &mut Criterion) {
    let mut group = c.benchmark_group("custom_delimiter");
    for delimiter in ['%', '#'] {
        let template = generate_template_with_blocks(delimiter);
        let options = MinifyOptions { delimiter, ..MinifyOptions::default() };
        group.throughput(Throughput::Bytes(template.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("native", delimiter),
            &template,
            |b, template| b.iter(|| black_box(options.minify_native(black_box(template)).unwrap())),
        );
    }
    group.finish();
}

#[cfg(not(feature = "native-minifier"))]
fn bench_custom_delimiter(_: &mut Criterion) {}

criterion_group!(
    benches,
    bench_custom_delimiter,
    bench_derive_input,
    bench_duplicate_includes,
    bench_template_sizes,
    bench_include_depth,
    bench_parallel_includes,
    bench_cache_hit_vs_miss,
    bench_include_parse,
    bench_include_parse_large,
    bench_include_replace
);
criterion_main!(benches);
