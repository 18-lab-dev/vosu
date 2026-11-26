//! Page shells. Each shell carries only presentation: the heading, current
//! tab, search hint and static notices. Lists, counts, settings values and
//! the update notice come from core's JSON routes after load, and core injects
//! the page context at [`CONTEXT_MARKER`].

/// Core replaces the first occurrence with the page-context script (A7.1).
pub const CONTEXT_MARKER: &str = "<!--vosu-context-->";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Overlays,
    Available,
    Tools,
    Settings,
    /// `/settings?overlay=…`: one overlay's settings, rendered by the client.
    OverlaySettings,
    /// `/settings?tool=…`: one tool's settings, rendered by the client.
    ToolSettings,
}

impl Page {
    /// The page for a dashboard path and its decoded query, as the client
    /// reads it: a named overlay wins over a named tool.
    pub fn for_request(path: &str, query: &[(String, String)]) -> Option<Self> {
        let named = |key: &str| {
            query
                .iter()
                .any(|(name, value)| name == key && !value.is_empty())
        };
        Some(match path {
            "/" => Self::Overlays,
            "/available" => Self::Available,
            "/tools" => Self::Tools,
            "/settings" if named("overlay") => Self::OverlaySettings,
            "/settings" if named("tool") => Self::ToolSettings,
            "/settings" => Self::Settings,
            _ => return None,
        })
    }

    /// `data-page` of the current tab, heading and search placeholder.
    fn chrome(self) -> (&'static str, &'static str, Option<&'static str>) {
        match self {
            Self::Overlays => ("installed", "Overlays", Some("Search overlays")),
            Self::Available => (
                "available",
                "Available overlays",
                Some("Search available overlays"),
            ),
            Self::Tools => ("tools", "Tools", Some("Search tools")),
            Self::Settings => ("settings", "Settings", Some("Search settings")),
            Self::OverlaySettings => ("installed", "Overlay settings", None),
            Self::ToolSettings => ("tools", "Tool settings", None),
        }
    }
}

/// Third-party disclosure for the Available page. It sits outside the results
/// section so catalog searches, which replace only the results, keep it.
const CATALOG_NOTICE: &str = concat!(
    r#"<details class="catalog-notice" open><summary>Community overlays · third-party code</summary>"#,
    r#"<p>Third-party JavaScript from <a href="https://tosu.app" target="_blank" rel="noopener noreferrer">tosu.app</a> (downloads via osuck.link), not reviewed by vosu. Install only overlays you trust.</p>"#,
    "</details>",
);

/// Shown until the list arrives from `/api/counters/search/`.
fn loading(message: &str) -> String {
    format!(r#"<div class="empty-state results-loading" role="status"><p>{message}</p></div>"#)
}

fn search_form(placeholder: &str) -> String {
    format!(
        r#"<form class="search-form" role="search"><div class="search-control"><input id="counter-search" class="search-bar" type="search" autocomplete="off" placeholder="{placeholder}" aria-label="{placeholder}" aria-controls="counter-results"><kbd aria-hidden="true">/</kbd></div></form>"#
    )
}

pub fn render(page: Page, version: &str) -> String {
    let (key, title, search) = page.chrome();
    let version = escape_html(version);
    let search = search.map(search_form).unwrap_or_default();
    let list = match page {
        Page::Overlays => loading("Loading overlays…"),
        Page::Available => loading("Loading the community catalog…"),
        Page::Tools => r#"<div id="tools-root" class="tools-root"></div>"#.to_owned(),
        Page::Settings => crate::settings::form(&version),
        Page::OverlaySettings | Page::ToolSettings => String::new(),
    };
    let current = |tab: &str| {
        if key == tab {
            r#" aria-current="page""#
        } else {
            ""
        }
    };
    substitute_placeholders(crate::assets::HOMEPAGE, |name| {
        Some(match name {
            "PAGE_KEY" => key,
            "PAGE_TITLE" => title,
            "CURRENT_INSTALLED" => current("installed"),
            "CURRENT_AVAILABLE" => current("available"),
            "CURRENT_TOOLS" => current("tools"),
            "CURRENT_SETTINGS" => current("settings"),
            // Settings saves as you go: its save state shares the title row
            // and is empty at rest (homepage.js fills it).
            "PAGE_STATUS" if page == Page::Settings => r#"<p class="settings-status" hidden></p>"#,
            "PAGE_STATUS" => "",
            "SEARCH" => &search,
            "NOTICE" if page == Page::Available => CATALOG_NOTICE,
            "NOTICE" => "",
            "LIST" => &list,
            "CURRENT_VERSION" => &version,
            _ => return None,
        })
    })
}

/// Replaces `{{KEY}}` placeholders in a single pass. Substituted values are
/// never rescanned, so placeholder-shaped text is emitted verbatim.
fn substitute_placeholders<'a>(template: &str, value: impl Fn(&str) -> Option<&'a str>) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after
            .find("}}")
            .and_then(|end| Some((end, value(&after[..end])?)))
        {
            Some((end, replacement)) => {
                output.push_str(replacement);
                rest = &after[end + 2..];
            }
            None => {
                output.push_str("{{");
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

pub(crate) fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}
