/// Sanitize user-provided CSS to prevent injection attacks.
///
/// Strips:
/// - `@import` rules (prevent loading external stylesheets)
/// - `url()` values (prevent loading external resources)
/// - `javascript:` protocol (XSS)
/// - `expression()` (IE CSS expressions)
/// - `behavior:` property (IE HTC)
pub fn sanitize_css(css: &str) -> String {
    let mut result = String::with_capacity(css.len());
    for line in css.lines() {
        let trimmed = line.trim().to_lowercase();
        // Skip entire line if it starts with @import
        if trimmed.starts_with("@import") {
            continue;
        }
        // Skip lines containing dangerous constructs
        if trimmed.contains("url(")
            || trimmed.contains("javascript:")
            || trimmed.contains("expression(")
            || trimmed.contains("behavior:")
        {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

/// Sanitize user-provided HTML for slot fields (head_html, nav_extra_html, footer_html).
///
/// Strips dangerous elements and attributes:
/// - `<script>`, `<iframe>`, `<object>`, `<embed>` tags
/// - `on*` event handler attributes
/// - `javascript:` protocol URLs
/// - `data:text/html` URIs
pub fn sanitize_html(html: &str) -> String {
    use std::fmt::Write;

    let mut result = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            // Collect the tag
            let mut tag = String::from('<');
            for c in chars.by_ref() {
                tag.push(c);
                if c == '>' {
                    break;
                }
            }

            let tag_lower = tag.to_lowercase();
            let tag_name = tag_lower
                .trim_start_matches('<')
                .trim_start_matches('/')
                .trim()
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("");

            // Block dangerous tags
            if matches!(tag_name, "script" | "iframe" | "object" | "embed") {
                continue;
            }

            // Strip on* event handlers and dangerous protocols from attributes
            if tag_lower.contains("javascript:") || tag_lower.contains("data:text/html") {
                continue;
            }

            // Check for on* event handlers (onerror, onclick, etc.)
            let has_event_handler = tag_lower.split_whitespace().any(|attr| {
                let attr = attr.trim_start_matches('/').trim_end_matches('>');
                attr.starts_with("on") && attr.contains('=')
            });
            if has_event_handler {
                continue;
            }

            let _ = write!(result, "{}", tag);
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_css_strips_import() {
        let input = "@import url('evil.css');\n.safe { color: red; }";
        let result = sanitize_css(input);
        assert!(!result.contains("@import"));
        assert!(result.contains(".safe { color: red; }"));
    }

    #[test]
    fn sanitize_css_strips_url() {
        let input = ".bg { background: url('http://evil.com/img.png'); }";
        let result = sanitize_css(input);
        assert!(!result.contains("url("));
    }

    #[test]
    fn sanitize_css_strips_javascript() {
        let input = ".xss { background: javascript:alert(1); }";
        let result = sanitize_css(input);
        assert!(!result.contains("javascript:"));
    }

    #[test]
    fn sanitize_css_strips_expression() {
        let input = ".ie { width: expression(document.body.clientWidth); }";
        let result = sanitize_css(input);
        assert!(!result.contains("expression("));
    }

    #[test]
    fn sanitize_css_strips_behavior() {
        let input = ".htc { behavior: url(evil.htc); }";
        let result = sanitize_css(input);
        assert!(!result.contains("behavior:"));
    }

    #[test]
    fn sanitize_html_strips_script_tags() {
        let input = "<div>Hello</div><script>alert(1)</script><p>World</p>";
        let result = sanitize_html(input);
        assert!(!result.contains("<script>"));
        assert!(result.contains("<div>Hello</div>"));
        assert!(result.contains("<p>World</p>"));
    }

    #[test]
    fn sanitize_html_strips_iframe() {
        let input = r#"<iframe src="evil.com"></iframe><p>safe</p>"#;
        let result = sanitize_html(input);
        assert!(!result.contains("<iframe"));
        assert!(result.contains("<p>safe</p>"));
    }

    #[test]
    fn sanitize_html_strips_event_handlers() {
        let input = r#"<img onerror="alert(1)" src="x.png">"#;
        let result = sanitize_html(input);
        assert!(!result.contains("onerror"));
    }

    #[test]
    fn sanitize_html_strips_javascript_protocol() {
        let input = r#"<a href="javascript:alert(1)">click</a>"#;
        let result = sanitize_html(input);
        assert!(!result.contains("javascript:"));
    }

    #[test]
    fn sanitize_html_preserves_safe_content() {
        let input =
            r##"<link rel="icon" href="/favicon.ico"><meta name="theme-color" content="#333">"##;
        let result = sanitize_html(input);
        assert!(result.contains("<link"));
        assert!(result.contains("<meta"));
    }

    #[test]
    fn sanitize_html_strips_data_uri() {
        let input = r#"<a href="data:text/html,<script>alert(1)</script>">bad</a>"#;
        let result = sanitize_html(input);
        assert!(!result.contains("data:text/html"));
    }

    #[test]
    fn sanitize_css_preserves_safe_css() {
        let input =
            ".sf-custom {\n    color: #333;\n    font-size: 14px;\n    border: 1px solid #ccc;\n}";
        let result = sanitize_css(input);
        assert!(result.contains("color: #333;"));
        assert!(result.contains("font-size: 14px;"));
    }
}
