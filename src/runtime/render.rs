use super::*;

/// Per-page chrome rendered on the server, so the first paint already shows
/// the right heading, current tab and search hint.
struct PageChrome {
    /// `data-page` of the current navigation tab.
    key: &'static str,
    title: &'static str,
    /// Search placeholder and label; `None` renders no search field.
    search: Option<&'static str>,
}

fn page_chrome(page: DynamicPage) -> PageChrome {
    let (key, title, search) = match page {
        DynamicPage::Dashboard => ("installed", "Overlays", Some("Search overlays")),
        DynamicPage::Available => (
            "available",
            "Available overlays",
            Some("Search available overlays"),
        ),
        DynamicPage::Tools => ("tools", "Tools", Some("Search tools")),
        DynamicPage::Settings => ("settings", "Settings", Some("Search settings")),
        DynamicPage::OverlaySettings => ("installed", "Overlay settings", None),
        DynamicPage::ToolSettings => ("tools", "Tool settings", None),
    };
    PageChrome { key, title, search }
}

fn search_form(placeholder: &str) -> String {
    format!(
        r#"<form class="search-form" role="search"><div class="search-control"><input id="counter-search" class="search-bar" type="search" autocomplete="off" placeholder="{placeholder}" aria-label="{placeholder}" aria-controls="counter-results"><kbd aria-hidden="true">/</kbd></div></form>"#
    )
}

/// Third-party disclosure for the Available page. It sits outside the results
/// section so catalog searches, which replace only the results, keep it.
pub(super) const CATALOG_NOTICE: &str = concat!(
    r#"<details class="catalog-notice" open><summary>Community overlays · third-party code</summary>"#,
    r#"<p>Third-party JavaScript from <a href="https://tosu.app" target="_blank" rel="noopener noreferrer">tosu.app</a> (downloads via osuck.link), not reviewed by vosu. Install only overlays you trust.</p>"#,
    "</details>",
);

/// The update the dashboard advertises. A major release is installed only
/// after the dashboard's explicit confirmation, so it renders differently.
#[derive(Clone)]
pub(super) enum UpdateNotice {
    Minor(String),
    Major(vosu_core::updater::MajorRelease),
}

pub(super) fn render_homepage(
    template: EmbeddedResponse,
    page: DynamicPage,
    local_count: Option<usize>,
    available_count: Option<usize>,
    notice: &str,
    list: &str,
    update: Option<&UpdateNotice>,
) -> EmbeddedResponse {
    let chrome = page_chrome(page);
    let local_count = local_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let available_count = available_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let current_version = current_version().to_string();
    let search = chrome.search.map(search_form).unwrap_or_default();
    // Settings saves as you go: its save state shares the title row and is
    // empty at rest (homepage.js fills it after a save or while edits wait).
    let status = if page == DynamicPage::Settings {
        r#"<p class="settings-status" hidden></p>"#
    } else {
        ""
    };
    let current = |key: &str| {
        if chrome.key == key {
            r#" aria-current="page""#
        } else {
            ""
        }
    };
    let (new_version, major) = match update {
        Some(UpdateNotice::Minor(version)) => (version.clone(), None),
        Some(UpdateNotice::Major(release)) => (release.version.to_string(), Some(release)),
        None => (String::new(), None),
    };
    let release_url = major
        .map(|release| escape_html(&release.release_url))
        .unwrap_or_default();
    let body = substitute_placeholders(&String::from_utf8_lossy(&template.body), |key| {
        Some(match key {
            "LOCAL_AMOUNT" => &local_count,
            "AVAILABLE_AMOUNT" => &available_count,
            "PAGE_KEY" => chrome.key,
            "PAGE_TITLE" => chrome.title,
            "CURRENT_INSTALLED" => current("installed"),
            "CURRENT_AVAILABLE" => current("available"),
            "CURRENT_TOOLS" => current("tools"),
            "CURRENT_SETTINGS" => current("settings"),
            "PAGE_STATUS" => status,
            "SEARCH" => &search,
            "NOTICE" => notice,
            "LIST" => list,
            "UPDATE_HIDDEN" if update.is_some() => "",
            "UPDATE_HIDDEN" => "hidden",
            "MINOR_NOTICE_HIDDEN" if major.is_none() => "",
            "MINOR_NOTICE_HIDDEN" => "hidden",
            "MAJOR_NOTICE_HIDDEN" if major.is_some() => "",
            "MAJOR_NOTICE_HIDDEN" => "hidden",
            "UPDATE_IS_MAJOR" if major.is_some() => "true",
            "UPDATE_IS_MAJOR" => "false",
            "RELEASE_URL" => &release_url,
            "CURRENT_VERSION" => &current_version,
            "NEW_VERSION" => &new_version,
            _ => return None,
        })
    });
    EmbeddedResponse {
        body: Bytes::from(body),
        ..template
    }
}

/// Replaces `{{KEY}}` placeholders in a single pass. Substituted values are
/// never rescanned, so placeholder-shaped text in overlay metadata or catalog
/// content is emitted verbatim instead of being rewritten by a later key.
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

include!("render/settings.rs");
pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

