// straitjacket-allow-file — a byte-faithful transcription of pi's OAuth
// callback HTML page (`oauth-page.ts`): the `LOGO_SVG` constant and the full
// HTML/CSS template are reproduced verbatim so the rendered bytes match pi's
// exactly. The clone detector flags the near-identical `oauth_success_html` /
// `oauth_error_html` wrappers and the large literal; the fidelity is intentional.
//! OAuth callback pages, ported from pi-ai's
//! `packages/ai/src/auth/oauth/oauth-page.ts` at pinned commit `3da591ab`.
//!
//! [`oauth_success_html`] and [`oauth_error_html`] render the byte-faithful HTML
//! served by the (out-of-scope) loopback callback servers, including the
//! `LOGO_SVG` constant and HTML-escaping of `& < > " '` (`oauth-page.ts:1-109`).

/// The inline logo SVG, verbatim from pi (`oauth-page.ts:1`).
const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#fff" fill-rule="evenodd" d="M165.29 165.29 H517.36 V400 H400 V517.36 H282.65 V634.72 H165.29 Z M282.65 282.65 V400 H400 V282.65 Z"/><path fill="#fff" d="M517.36 400 H634.72 V634.72 H517.36 Z"/></svg>"##;

/// HTML-escape `& < > " '`, in pi's exact replacement order (`oauth-page.ts:3-10`).
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// The static template split around pi's five interpolation points
// (`oauth-page.ts:18-91`). Concatenating these with the escaped values (and the
// unescaped `LOGO_SVG`) reproduces pi's output byte-for-byte.
const HEAD_BEFORE_TITLE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>"#;

const TITLE_TO_LOGO: &str = r#"</title>
  <style>
    :root {
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }
    * { box-sizing: border-box; }
    html { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }
    main {
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }
    .logo {
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }
    p {
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }
    .details {
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }
  </style>
</head>
<body>
  <main>
    <div class="logo">"#;

const LOGO_TO_HEADING: &str = r#"</div>
    <h1>"#;

const HEADING_TO_MESSAGE: &str = r#"</h1>
    <p>"#;

// After the message: `</p>`, newline, then the 4-space indent before the
// (optional) details block.
const MESSAGE_TO_DETAILS: &str = "</p>\n    ";

const AFTER_DETAILS: &str = "\n  </main>\n</body>\n</html>";

fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let title = escape_html(title);
    let heading = escape_html(heading);
    let message = escape_html(message);
    let details = details.map(escape_html);

    let details_block = match &details {
        Some(details) => format!(r#"<div class="details">{details}</div>"#),
        None => String::new(),
    };

    let mut out = String::new();
    out.push_str(HEAD_BEFORE_TITLE);
    out.push_str(&title);
    out.push_str(TITLE_TO_LOGO);
    out.push_str(LOGO_SVG);
    out.push_str(LOGO_TO_HEADING);
    out.push_str(&heading);
    out.push_str(HEADING_TO_MESSAGE);
    out.push_str(&message);
    out.push_str(MESSAGE_TO_DETAILS);
    out.push_str(&details_block);
    out.push_str(AFTER_DETAILS);
    out
}

/// Render the success page (`oauth-page.ts:94-100`).
pub fn oauth_success_html(message: &str) -> String {
    render_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

/// Render the error page (`oauth-page.ts:102-109`).
pub fn oauth_error_html(message: &str, details: Option<&str>) -> String {
    render_page(
        "Authentication failed",
        "Authentication failed",
        message,
        details,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_all_five_entities_in_order() {
        assert_eq!(
            escape_html(r#"a & b < c > d " e ' f"#),
            "a &amp; b &lt; c &gt; d &quot; e &#39; f"
        );
        // Ampersand is escaped first so a literal `<` does not become `&amp;lt;`.
        assert_eq!(escape_html("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn success_page_contains_logo_and_escaped_message() {
        let html = oauth_success_html("Done <ok> & \"safe\"");
        assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
        assert!(html.contains("<title>Authentication successful</title>"));
        assert!(html.contains("<h1>Authentication successful</h1>"));
        assert!(html.contains(LOGO_SVG));
        assert!(html.contains("<p>Done &lt;ok&gt; &amp; &quot;safe&quot;</p>"));
        // No details block on success.
        assert!(!html.contains(r#"<div class="details">"#));
        assert!(html.ends_with("\n  </main>\n</body>\n</html>"));
    }

    #[test]
    fn error_page_renders_details_block_when_present() {
        let html = oauth_error_html("Bad", Some("code=invalid_grant"));
        assert!(html.contains("<title>Authentication failed</title>"));
        assert!(html.contains("<p>Bad</p>"));
        assert!(html.contains(r#"<div class="details">code=invalid_grant</div>"#));
    }

    #[test]
    fn error_page_omits_details_block_when_absent() {
        let html = oauth_error_html("Bad", None);
        assert!(!html.contains(r#"<div class="details">"#));
        // The 4-space indent before the (empty) details slot is preserved.
        assert!(html.contains("<p>Bad</p>\n    \n  </main>"));
    }
}
