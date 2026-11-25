use super::*;

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>"#;

#[test]
fn content_decides_the_type_and_the_extension_must_agree() {
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    let webp = b"RIFF\x10\0\0\0WEBPVP8 ";
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF";
    for (body, kind) in [
        (SVG, ArtKind::Svg),
        (&png[..], ArtKind::Png),
        (&webp[..], ArtKind::Webp),
        (&jpeg[..], ArtKind::Jpeg),
    ] {
        assert_eq!(sniff(body), Some(kind));
    }
    // Prologue forms real SVG files start with.
    for prologue in [
        "\u{feff}<?xml version=\"1.0\"?>\n<!-- art -->\n",
        "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">",
        "  \n",
    ] {
        let text = [prologue.as_bytes(), SVG].concat();
        assert_eq!(sniff(&text), Some(ArtKind::Svg), "{prologue}");
    }
    for body in [
        &b"<html><svg></svg></html>"[..],
        b"<svgx>",
        b"<!DOCTYPE svg [<!ENTITY a \"b\">]><svg/>",
        b"<!-- unterminated <svg>",
        b"GIF89a",
        b"\xff\xfe<\0s\0v\0g\0",
        b"",
    ] {
        assert_eq!(sniff(body), None, "{}", String::from_utf8_lossy(body));
    }
    assert_eq!(
        classify("https://example.com/a/icon.svg", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/a/ICON.SVG?x=1", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/download", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/a.jpeg", jpeg),
        Ok(ArtKind::Jpeg)
    );
    assert!(classify("https://example.com/a.png", SVG).is_err());
    assert!(classify("https://example.com/a.gif", SVG).is_err());
    assert!(classify("https://example.com/a.svg", b"<html></html>").is_err());
}

#[test]
fn cache_is_bounded_and_dropped_with_the_catalog_revision() {
    let mut cache = ArtCache::default();
    let first = 1;
    assert!(cache.get(first, "a").is_none());
    let big = Bytes::from(vec![0; 1024 * 1024]);
    for index in 0..20 {
        cache.insert(first, format!("{index}"), ArtKind::Png, big.clone());
    }
    assert!(cache.bytes <= CACHE_BYTES);
    assert_eq!(cache.len(), CACHE_BYTES / big.len());
    // Least recently used goes first.
    assert!(cache.get(first, "0").is_none());
    assert!(cache.get(first, "19").is_some());
    for index in 0..300 {
        cache.insert(
            first,
            format!("small-{index}"),
            ArtKind::Svg,
            Bytes::from_static(b"x"),
        );
    }
    assert_eq!(cache.len(), CACHE_ENTRIES);
    // Another revision starts empty and late inserts for the old one are dropped.
    assert!(cache.get(2, "small-299").is_none());
    assert_eq!((cache.len(), cache.bytes), (0, 0));
    cache.insert(first, "late".into(), ArtKind::Svg, Bytes::from_static(b"x"));
    assert_eq!(cache.len(), 0);
}
