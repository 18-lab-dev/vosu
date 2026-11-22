fn dashboard_with(update: Option<&UpdateNotice>) -> String {
    let page = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Dashboard,
        Some(0),
        Some(0),
        "",
        "",
        update,
    );
    String::from_utf8(page.body.to_vec()).unwrap()
}

#[test]
fn minor_and_major_update_notices_render_distinctly() {
    let minor = dashboard_with(Some(&UpdateNotice::Minor("1.9.0".to_owned())));
    assert!(minor.contains(r#"data-major="false""#));
    assert!(minor.contains(r#"<div class="update-notice__major" hidden>"#));
    assert!(minor.contains("<strong>vosu 1.9.0</strong> is available"));

    let major = dashboard_with(Some(&UpdateNotice::Major(vosu_core::updater::MajorRelease {
        version: semver::Version::new(2, 0, 0),
        release_url: "https://github.com/18-lab-dev/vosu/releases/tag/v2.0.0".to_owned(),
    })));
    assert!(major.contains(r#"data-major="true""#));
    assert!(!major.contains(r#"data-action="show-update" hidden"#));
    assert!(major.contains(r#"<p class="update-notice__copy" hidden>"#));
    assert!(major.contains("vosu 2.0.0 is available"));
    assert!(major.contains(r#"href="https://github.com/18-lab-dev/vosu/releases/tag/v2.0.0""#));
    assert!(major.contains(r#"data-role="major-update-consent""#));
}

#[test]
fn major_update_release_url_is_escaped_into_its_attribute() {
    let page = dashboard_with(Some(&UpdateNotice::Major(vosu_core::updater::MajorRelease {
        version: semver::Version::new(2, 0, 0),
        release_url: r#"https://example.com/"><script>alert(1)</script>"#.to_owned(),
    })));
    assert!(!page.contains("<script>alert(1)</script>"));
    assert!(page.contains("&quot;&gt;&lt;script&gt;"));
}
