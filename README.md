# sailfish-minify
Hacky but simple minification support for sailfish, using html-minifier by default

# IMPORTANT!
By default, sailfish-minify DOES also minify its components, however if you want to disable this behavior you can compile without "minifiy-components".
Also, the components are minified with the "parent" template options, this behavior is however untested when there are multiple parents using the same component but using different minifier options.

## Example

```rust
use sailfish::TemplateSimple;

#[derive(Debug, sailfish_minify::TemplateSimple)]
#[templ(path = "test.stpl")] // Notice the use of templ instead of template
// #[min_with(HTMLMinifier)] // Default is HTMLMinifier anyway
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

