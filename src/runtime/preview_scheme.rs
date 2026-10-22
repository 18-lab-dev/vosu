//! The colour scheme an installed overlay declares for its root element.
//!
//! Browsers paint an opaque canvas behind an embedded document whose used
//! colour scheme differs from the used scheme of its `<iframe>` element, so a
//! dashboard preview is only transparent when the frame's `color-scheme`
//! matches the overlay's. Overlays are cross-origin to the dashboard, so the
//! dashboard cannot ask the document; vosu reads the declaration from the
//! overlay's `index.html` and the local stylesheets it links instead.
//!
//! Only static declarations are seen: `<meta name="color-scheme">`, a `style`
//! attribute on `<html>`, and `color-scheme` in unconditional `:root` / `html`
//! rules of inline `<style>` blocks and linked local stylesheets. A scheme set
//! from JavaScript keeps the light default.

use std::{
    fs,
    io::Read,
    path::{Component, Path},
};

/// Largest `index.html` or stylesheet read; larger files are read up to this.
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Linked stylesheets inspected per overlay.
const MAX_STYLESHEETS: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PreviewScheme {
    /// No declaration, `normal`, or a list that includes `light`. A
    /// `light dark` overlay follows its frame, so a light frame matches it.
    #[default]
    Light,
    /// Declares `dark` without `light`.
    Dark,
}

/// Reads the scheme of the overlay whose `index.html` lives in `directory`.
/// Unreadable or missing files leave the light default.
pub(super) fn overlay_preview_scheme(directory: &Path) -> PreviewScheme {
    let Some(html) = read_bounded(&directory.join("index.html")) else {
        return PreviewScheme::Light;
    };
    // Cascade order: the `<html style>` attribute beats root rules, and any
    // root rule beats the meta tag, which only fills in a `normal` root.
    let (mut meta, mut rule, mut inline) = (None, None, None);
    let mut stylesheets = 0;
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find('<') {
        let start = cursor + offset;
        let rest = &lower[start + 1..];
        if rest.starts_with("!--") {
            cursor = rest
                .find("-->")
                .map_or(lower.len(), |end| start + 1 + end + 3);
            continue;
        }
        let name_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &html[start + 1..start + 1 + tag_end];
        let after = start + 1 + tag_end + 1;
        cursor = after;
        match name {
            "meta" if meta.is_none() => {
                if attribute(tag, "name").is_some_and(|v| v.eq_ignore_ascii_case("color-scheme")) {
                    meta = attribute(tag, "content").map(str::to_owned);
                }
            }
            "html" => {
                if let Some(value) = attribute(tag, "style").and_then(declaration) {
                    inline = Some(value);
                }
            }
            "style" => {
                let end = lower[after..]
                    .find("</style")
                    .map_or(lower.len(), |end| after + end);
                if let Some(value) = root_declaration(&html[after..end]) {
                    rule = Some(value);
                }
                cursor = end;
            }
            "link" => {
                let stylesheet = attribute(tag, "rel").is_some_and(|rel| {
                    rel.split_ascii_whitespace()
                        .any(|token| token.eq_ignore_ascii_case("stylesheet"))
                });
                if stylesheet
                    && stylesheets < MAX_STYLESHEETS
                    && let Some(path) = attribute(tag, "href").and_then(local_path)
                {
                    stylesheets += 1;
                    if let Some(value) = confined_read(directory, path)
                        .as_deref()
                        .and_then(root_declaration)
                    {
                        rule = Some(value);
                    }
                }
            }
            _ => {}
        }
    }
    let root = inline
        .or(rule)
        .filter(|value| !value.eq_ignore_ascii_case("normal"));
    root.or(meta)
        .as_deref()
        .map_or(PreviewScheme::Light, parse_scheme)
}

fn parse_scheme(value: &str) -> PreviewScheme {
    let mut dark = false;
    for token in value.split_ascii_whitespace() {
        if token.eq_ignore_ascii_case("light") {
            return PreviewScheme::Light;
        }
        dark |= token.eq_ignore_ascii_case("dark");
    }
    if dark {
        PreviewScheme::Dark
    } else {
        PreviewScheme::Light
    }
}

/// Value of an HTML attribute in a start tag's source (without `<` and `>`).
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut index = tag
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(tag.len());
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'=' | b'/')
        {
            index += 1;
        }
        let key = &tag[key_start..index];
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let value = if bytes.get(index) == Some(&b'=') {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            match bytes.get(index) {
                Some(&quote @ (b'"' | b'\'')) => {
                    let start = index + 1;
                    let end = tag[start..]
                        .find(quote as char)
                        .map_or(tag.len(), |end| start + end);
                    index = end + 1;
                    &tag[start..end]
                }
                _ => {
                    let start = index;
                    while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                        index += 1;
                    }
                    &tag[start..index]
                }
            }
        } else {
            ""
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        if key.is_empty() {
            index += 1;
        }
    }
    None
}

/// The last `color-scheme` value in a declaration block.
fn declaration(block: &str) -> Option<String> {
    block.split(';').rev().find_map(|item| {
        let (property, value) = item.split_once(':')?;
        property
            .trim()
            .eq_ignore_ascii_case("color-scheme")
            .then(|| {
                let value = value.trim();
                let value = value
                    .strip_suffix("!important")
                    .or_else(|| value.strip_suffix("!IMPORTANT"))
                    .unwrap_or(value);
                value.trim().to_owned()
            })
    })
}

/// The last `color-scheme` declared by an unconditional `:root` or `html`
/// rule. Rules inside at-rules (`@media`, `@supports`, …) apply only under
/// their condition and are skipped.
fn root_declaration(css: &str) -> Option<String> {
    let css = strip_comments(css);
    let mut found = None;
    let mut depth = 0usize;
    let mut skip_depth = None;
    let mut boundary = 0;
    for (index, character) in css.char_indices() {
        match character {
            '{' => {
                let prelude = css[boundary..index].trim();
                if depth == 0 && skip_depth.is_none() && !prelude.starts_with('@') {
                    let close = css[index + 1..]
                        .find('}')
                        .map_or(css.len(), |end| index + 1 + end);
                    let block = &css[index + 1..close];
                    if prelude.split(',').any(|selector| {
                        let selector = selector.trim();
                        selector.eq_ignore_ascii_case(":root")
                            || selector.eq_ignore_ascii_case("html")
                    }) && let Some(value) = declaration(block)
                    {
                        found = Some(value);
                    }
                } else if skip_depth.is_none() {
                    skip_depth = Some(depth);
                }
                depth += 1;
                boundary = index + 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                boundary = index + 1;
            }
            ';' if depth == 0 => boundary = index + 1,
            _ => {}
        }
    }
    found
}

fn strip_comments(css: &str) -> std::borrow::Cow<'_, str> {
    if !css.contains("/*") {
        return std::borrow::Cow::Borrowed(css);
    }
    let mut output = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        output.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    output.push_str(rest);
    std::borrow::Cow::Owned(output)
}

/// A relative stylesheet reference inside the overlay folder. Absolute,
/// scheme-relative, encoded, and parent-relative references are not followed.
fn local_path(href: &str) -> Option<&Path> {
    let href = href.trim();
    let href = href.split(['?', '#']).next().unwrap_or_default();
    if href.is_empty() || href.starts_with('/') || href.contains([':', '%', '\\']) {
        return None;
    }
    let path = Path::new(href);
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
        .then_some(path)
}

fn confined_read(directory: &Path, relative: &Path) -> Option<String> {
    let root = fs::canonicalize(directory).ok()?;
    let target = fs::canonicalize(directory.join(relative)).ok()?;
    if !target.starts_with(&root) {
        return None;
    }
    read_bounded(&target)
}

fn read_bounded(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme(html: &str) -> PreviewScheme {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), html).unwrap();
        overlay_preview_scheme(root.path())
    }

    #[test]
    fn meta_and_root_rules_decide_the_preview_scheme() {
        for (html, expected) in [
            ("<html><body></body></html>", PreviewScheme::Light),
            (
                r#"<meta name="color-scheme" content="dark">"#,
                PreviewScheme::Dark,
            ),
            (
                r#"<META content='dark only' NAME=color-scheme>"#,
                PreviewScheme::Dark,
            ),
            (
                r#"<meta name="color-scheme" content="light dark">"#,
                PreviewScheme::Light,
            ),
            (
                r#"<meta name="color-scheme" content="light">"#,
                PreviewScheme::Light,
            ),
            (
                "<style>:root { color-scheme: dark; }</style>",
                PreviewScheme::Dark,
            ),
            (
                "<style>html, body { margin: 0; color-scheme: dark !important }</style>",
                PreviewScheme::Dark,
            ),
            // A `normal` root leaves the meta value in effect; a light root
            // rule overrides it.
            (
                r#"<meta name="color-scheme" content="dark"><style>:root{color-scheme:normal}</style>"#,
                PreviewScheme::Dark,
            ),
            (
                r#"<meta name="color-scheme" content="dark"><style>html{color-scheme:light}</style>"#,
                PreviewScheme::Light,
            ),
            (
                r#"<html style="color-scheme: dark"><body></body></html>"#,
                PreviewScheme::Dark,
            ),
            // Element rules, conditional rules, and comments do not set the root.
            (
                "<style>select { color-scheme: dark }</style>",
                PreviewScheme::Light,
            ),
            (
                "<style>@media (prefers-color-scheme: dark) { :root { color-scheme: dark } } body{}</style>",
                PreviewScheme::Light,
            ),
            (
                "<style>/* :root { color-scheme: dark } */</style>",
                PreviewScheme::Light,
            ),
            (
                r#"<!-- <meta name="color-scheme" content="dark"> -->"#,
                PreviewScheme::Light,
            ),
        ] {
            assert_eq!(scheme(html), expected, "{html}");
        }
    }

    #[test]
    fn linked_stylesheets_are_read_only_inside_the_overlay() {
        let parent = tempfile::tempdir().unwrap();
        let overlay = parent.path().join("overlay");
        fs::create_dir_all(overlay.join("assets")).unwrap();
        fs::write(
            parent.path().join("outside.css"),
            ":root{color-scheme:dark}",
        )
        .unwrap();
        fs::write(
            overlay.join("index.html"),
            r#"<link rel="stylesheet" href="../outside.css"><link href="/outside.css" rel=stylesheet>"#,
        )
        .unwrap();
        assert_eq!(overlay_preview_scheme(&overlay), PreviewScheme::Light);

        fs::write(
            overlay.join("assets/index-1a2b.css"),
            "html{color-scheme:dark}",
        )
        .unwrap();
        fs::write(
            overlay.join("index.html"),
            r#"<link rel="stylesheet" crossorigin href="./assets/index-1a2b.css?v=2">"#,
        )
        .unwrap();
        assert_eq!(overlay_preview_scheme(&overlay), PreviewScheme::Dark);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                parent.path().join("outside.css"),
                overlay.join("linked.css"),
            )
            .unwrap();
            fs::write(
                overlay.join("index.html"),
                r#"<link rel="stylesheet" href="linked.css">"#,
            )
            .unwrap();
            assert_eq!(overlay_preview_scheme(&overlay), PreviewScheme::Light);
        }
    }

    #[test]
    fn missing_index_keeps_the_light_default() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(overlay_preview_scheme(root.path()), PreviewScheme::Light);
    }
}
