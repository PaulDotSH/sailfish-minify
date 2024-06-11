# sailfish-minify
Hacky but simple minification support for sailfish, using html-minifier by default

## Example

```rust
use sailfish::TemplateOnce;

#[derive(Debug, sailfish_minify::TemplateOnce)]
#[templ(path = "test.stpl")] // Notice the use of templ instead of template
// #[min_with(HTMLMinifier)] // Default is HTMLMinifier anyway
// #[min_with(Custom(html-minifier --collapse-whitespace))] // You can even use custom commands
struct MinifiedTestTemplate<'a> {
    s: &'a str
}

#[derive(Debug, TemplateOnce)]
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