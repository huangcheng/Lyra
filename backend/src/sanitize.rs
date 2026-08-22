//! Sanitization of attacker-controlled email HTML before storage/rendering.
//!
//! Email HTML comes from remote senders and is rendered in the app origin, so
//! it must never carry active content. All `body_html` produced by the IMAP
//! and JMAP ingest paths passes through [`sanitize_email_html`].

use std::sync::LazyLock;

use ammonia::Builder;

/// Shared sanitizer instance; configuration is static so it is built once.
static SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    // Start from ammonia's safe defaults: they allow typical email formatting
    // (p, div, span, br, b/i/em/strong, a, img, tables, lists, headings,
    // blockquote, pre, code) and already drop `<script>`, `<iframe>`,
    // `<object>`, `<embed>`, `<form>`, `<meta>`, `<link>`, `<base>`, event
    // handler attributes (`on*`), and the `style` attribute.
    let mut builder = Builder::default();

    // URL scheme whitelist: no `javascript:`/`vbscript:`/`file:`.
    // `data:` is intentionally excluded: ammonia cannot restrict it to
    // `data:image/*`, so embedded data URIs are dropped (safest option).
    builder.url_schemes(
        [
            "http", "https", "mailto", "tel", "ftp", "ftps", "cid", "mid",
        ]
        .into_iter()
        .collect(),
    );

    // Email HTML frequently uses `align`/presentational attributes; keep the
    // defaults but make sure `<img>` keeps only safe attributes.
    builder.add_tag_attributes("img", ["src", "alt", "title", "width", "height"]);
    builder.add_tag_attributes("a", ["href", "title", "target"]);

    builder
});

/// Sanitize an HTML email body, stripping scripts, event handlers, dangerous
/// URL schemes, and active/embed content while preserving formatting.
pub fn sanitize_email_html(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_script_tags() {
        let out = sanitize_email_html("<p>hi</p><script>alert(1)</script>");
        assert!(!out.contains("<script"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
        assert!(out.contains("<p>hi</p>"), "got: {out}");
    }

    #[test]
    fn strips_event_handlers() {
        let out = sanitize_email_html("<img src=\"https://x/y.png\" onerror=\"alert(1)\">");
        assert!(!out.to_lowercase().contains("onerror"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
        assert!(out.contains("src=\"https://x/y.png\""), "got: {out}");
    }

    #[test]
    fn neutralizes_javascript_urls() {
        let out = sanitize_email_html("<a href=\"javascript:alert(1)\">click</a>");
        assert!(!out.to_lowercase().contains("javascript:"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
        assert!(out.contains(">click</a>"), "got: {out}");
    }

    #[test]
    fn strips_svg_active_content() {
        let out = sanitize_email_html("<svg onload=\"alert(1)\"></svg>");
        assert!(!out.to_lowercase().contains("onload"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
    }

    #[test]
    fn removes_iframe_object_embed_form_meta_link_base() {
        let html = "<iframe src=\"https://evil\"></iframe>\
                    <object data=\"x\"></object>\
                    <embed src=\"x\">\
                    <form action=\"https://evil\"></form>\
                    <meta http-equiv=\"refresh\" content=\"0;url=https://evil\">\
                    <link rel=\"stylesheet\" href=\"https://evil/x.css\">\
                    <base href=\"https://evil/\">\
                    <p>body</p>";
        let out = sanitize_email_html(html);
        for tag in [
            "<iframe", "<object", "<embed", "<form", "<meta", "<link", "<base",
        ] {
            assert!(!out.contains(tag), "{tag} survived: {out}");
        }
        assert!(out.contains("<p>body</p>"), "got: {out}");
    }

    #[test]
    fn strips_data_uris() {
        let out = sanitize_email_html(
            "<img src=\"data:image/png;base64,iVBORw0KGgo=\"><a href=\"data:text/html,x\">y</a>",
        );
        assert!(!out.contains("data:"), "got: {out}");
    }

    #[test]
    fn preserves_legitimate_formatting() {
        let html = "<h1>Title</h1><p><b>hello</b> <em>world</em></p>\
                    <blockquote><pre><code>fn main() {}</code></pre></blockquote>\
                    <ul><li>one</li><li>two</li></ul>\
                    <table><thead><tr><th>H</th></tr></thead>\
                    <tbody><tr><td>cell</td></tr></tbody></table>\
                    <a href=\"https://example.com/path\">link</a>";
        let out = sanitize_email_html(html);
        for keep in [
            "<h1>Title</h1>",
            "<b>hello</b>",
            "<em>world</em>",
            "<blockquote>",
            "<pre>",
            "<code>fn main() {}</code>",
            "<ul>",
            "<li>one</li>",
            "<table>",
            "<td>cell</td>",
            "href=\"https://example.com/path\"",
        ] {
            assert!(out.contains(keep), "missing {keep} in: {out}");
        }
    }

    #[test]
    fn strips_style_attribute() {
        let out =
            sanitize_email_html("<p style=\"background:url(javascript:alert(1))\">x</p>");
        assert!(!out.contains("style="), "got: {out}");
    }
}
