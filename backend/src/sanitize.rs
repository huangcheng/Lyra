//! Sanitization of attacker-controlled email HTML before storage/rendering.
//!
//! Email HTML comes from remote senders and is rendered in the app origin, so
//! it must never carry active content. All `body_html` written to storage
//! passes through [`persist_body_html`] (IMAP, JMAP, and lazy body fetch).
//!
//! Note: `cid:` URLs are allowed here so inline-image references survive, but
//! actually rendering them additionally needs `img-src cid:` in the CSP (or
//! rewriting to served attachment URLs) if inline-image rendering is ever
//! enabled.

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

/// HTML about to be stored on `message.body_html`. `None` stays `None`.
#[must_use]
pub fn persist_body_html(html: Option<&str>) -> Option<String> {
    html.map(sanitize_email_html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_body_html_maps_none_and_sanitizes() {
        assert!(persist_body_html(None).is_none());
        let out = persist_body_html(Some(
            "<p>hi</p><script>alert(1)</script><img src=\"https://x/y.png\" onerror=\"alert(2)\">",
        ))
        .expect("html");
        assert!(out.contains("<p>hi</p>"), "got: {out}");
        assert!(!out.contains("<script"), "got: {out}");
        assert!(!out.to_lowercase().contains("onerror"), "got: {out}");
        assert!(!out.contains("alert("), "got: {out}");
        assert!(out.contains("src=\"https://x/y.png\""), "got: {out}");
    }

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
        let out = sanitize_email_html("<p style=\"background:url(javascript:alert(1))\">x</p>");
        assert!(!out.contains("style="), "got: {out}");
    }

    #[test]
    fn preserves_cid_and_mailto_urls() {
        // Regression lock for the custom scheme whitelist.
        let html = "<img src=\"cid:part1@example\">\
                    <a href=\"mailto:someone@example.com\">mail</a>";
        let out = sanitize_email_html(html);
        assert!(out.contains("src=\"cid:part1@example\""), "got: {out}");
        assert!(
            out.contains("href=\"mailto:someone@example.com\""),
            "got: {out}"
        );
    }

    #[test]
    fn strips_uppercase_script_and_entity_obfuscated_urls() {
        let out = sanitize_email_html("<SCRIPT>alert(1)</SCRIPT><p>ok</p>");
        assert!(!out.to_lowercase().contains("<script"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
        assert!(out.contains("<p>ok</p>"), "got: {out}");

        let out = sanitize_email_html("<a href=\"jav&#x61;script:alert(1)\">x</a>");
        assert!(!out.to_lowercase().contains("javascript:"), "got: {out}");
        assert!(!out.contains("alert(1)"), "got: {out}");
    }
}
