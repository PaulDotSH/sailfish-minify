# sailfish-minify

Minification support for [sailfish](https://github.com/rust-sailfish/sailfish).

By default templates are minified with the pure-Rust [`minify-html`](https://crates.io/crates/minify-html)
minifier, which runs in-process, requires no system tools, and is WASM-compatible.
Shelling out to the `html-minifier` CLI (or any custom command) is supported as an
opt-in alternative.

# Minifiers

| Minifier            | Description                                                                    | Default |
|---------------------|--------------------------------------------------------------------------------|---------|
| `Native`            | Pure-Rust, in-process, no system dependency, WASM-compatible.    | yes     |
| `HTMLMinifier`      | Shells out to the `html-minifier` CLI (requires Node.js / npm).                | no      |
| `Custom(...)`       | Shells out to an arbitrary command; panics if the command writes to stderr.    | no      |
| `CustomUnchecked(...)` | Shells out to an arbitrary command; stderr is ignored.                       | no      |

# Requirements

The default `Native` minifier has no requirements.

The `html-minifier` CLI is only required if you opt in with
`#[min_with(HTMLMinifier)]` or a `Custom`/`CustomUnchecked` command. It must be
installed and on your `PATH` (e.g. `npm install -g html-minifier`). On Windows the
npm `.cmd` shim is resolved automatically.

# Features

| Feature             | Description                                                                 | Default |
|---------------------|-----------------------------------------------------------------------------|---------|
| `native-minifier`   | Uses the `minify-html` crate.            | yes     |
| `minify-components` | Also minifies templates pulled in via `<% include!(...); %>`.                | yes     |
| `regex`             | Uses the `regex` crate to parse `<% include!(...); %>` instead of the faster dependency-free manual parser. | no      |


With `native-minifier` disabled, the default minifier falls back to the
`html-minifier` CLI.

# sailfish.toml

Templates are looked up the same way sailfish does it: in the `template_dirs` from your
`sailfish.toml`, then in `templates/` next to your `Cargo.toml`. A `delimiter` set there
is used unless the struct sets its own.

# IMPORTANT!
By default, sailfish-minify DOES also minify its components, however if you want to disable this behavior you can compile without "minifiy-components".
Also, the components are minified with the "parent" template options, this behavior is however untested when there are multiple parents using the same component but using different minifier options.

## Example

```rust
use sailfish::TemplateSimple;

#[derive(Debug, sailfish_minify::TemplateSimple)]
#[template(path = "test.stpl")] // Notice the use of templ instead of template
// #[min_with(HTMLMinifier)] // Opt-in: minify with the html-minifier CLI instead
// #[min_with(Native)]       // Default, pure-Rust minification (minify-html)
// #[min_with(Custom(html-minifier --collapse-whitespace))] // You can even use custom commands
struct MinifiedTestTemplate<'a> {
    s: &'a str
}

#[derive(Debug, TemplateSimple)]
#[template(path = "test.stpl")]
struct TestTemplate<'a> {
    s: &'a str
}

fn main() {
    println!("Unminified size: {} chars", TestTemplate { s: "test" }.render_once().unwrap().len());
    println!("Minified size: {} chars", MinifiedTestTemplate { s: "test" }.render_once().unwrap().len());
}
```

Output
```
Unminified size: 2238 chars
Minified size: 23 chars
```

## Performance

The `Native` minifier avoids the process-spawn overhead of shelling out to the CLI and is faster on every input size (Criterion benchmark, MiB/s):

Example on my hardware:
| Input | html-minifier CLI | Native (minify-html) |
|-------|-------------------|----------------------|
| 1 MB  | ~4 MiB/s          | ~30 MiB/s            |
| 10 MB | ~7 MiB/s          | ~32 MiB/s            |

It also produces equal-or-smaller output than `html-minifier --collapse-whitespace`
on typical sailfish templates, and does not auto-complete partial templates the way
`html-minifier` does.

Same happens for manual parsing of includes compared to regex, however the difference is extremely small in practice.