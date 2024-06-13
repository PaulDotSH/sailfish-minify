# sailfish-minify
Hacky but simple minification support for sailfish, using html-minifier by default

# IMPORTANT!
By default, sailfish-minify DOES also minify it's components, however if you want to disable this behavior, you can add the feature "minclude", which only minifies an included file if you use minclude!() instead of include!()
However you need to have this exact syntax, since it's not a real macro

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

