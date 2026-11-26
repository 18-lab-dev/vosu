//! Byte-range planning and the overlay media types, identical to core's
//! file routes: one RFC 9110 range, multiple ranges rejected, an end beyond
//! EOF unsatisfiable rather than clamped (upstream compatibility).

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ByteRange {
    Full,
    Partial { start: u64, end_inclusive: u64 },
}

/// Every malformed, multiple or unsatisfiable range is answered 416.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RangeRejected;

pub(crate) fn parse_range(header: Option<&str>, length: u64) -> Result<ByteRange, RangeRejected> {
    let Some(header) = header else {
        return Ok(ByteRange::Full);
    };
    let specification = header.trim().strip_prefix("bytes=").ok_or(RangeRejected)?;
    if specification.is_empty()
        || specification.contains(',')
        || specification.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(RangeRejected);
    }
    let (start, end) = specification.split_once('-').ok_or(RangeRejected)?;
    if end.contains('-') || length == 0 {
        return Err(RangeRejected);
    }
    if start.is_empty() {
        let suffix = decimal(end)?;
        if suffix == 0 {
            return Err(RangeRejected);
        }
        let suffix = suffix.min(length);
        return Ok(ByteRange::Partial {
            start: length - suffix,
            end_inclusive: length - 1,
        });
    }
    let start = decimal(start)?;
    if start >= length {
        return Err(RangeRejected);
    }
    let end_inclusive = if end.is_empty() {
        length - 1
    } else {
        let end = decimal(end)?;
        if end >= length || end < start {
            return Err(RangeRejected);
        }
        end
    };
    Ok(ByteRange::Partial {
        start,
        end_inclusive,
    })
}

fn decimal(value: &str) -> Result<u64, RangeRejected> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RangeRejected);
    }
    value.parse().map_err(|_| RangeRejected)
}

fn extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or_default();
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot + 1..],
        _ => "",
    }
}

pub(crate) fn media_range_eligible(path: &str) -> bool {
    matches!(
        extension(path),
        "mp3" | "wav" | "ogg" | "gif" | "webm" | "mp4" | "avi" | "webp"
    )
}

pub(crate) fn content_type(path: &str) -> &'static str {
    match extension(path) {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png; charset=utf-8",
        "jpg" => "image/jpg; charset=utf-8",
        "jpeg" => "image/jpeg; charset=utf-8",
        "gif" => "image/gif; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "wav" => "audio/wav; charset=utf-8",
        "mp4" => "video/mp4; charset=utf-8",
        "woff" => "application/font-woff; charset=utf-8",
        "woff2" => "font/woff2; charset=utf-8",
        "ttf" => "application/font-ttf; charset=utf-8",
        "eot" => "application/vnd.ms-fontobject; charset=utf-8",
        "otf" => "application/font-otf; charset=utf-8",
        "wasm" => "application/wasm; charset=utf-8",
        "aac" => "audio/aac; charset=utf-8",
        "abw" => "application/x-abiword; charset=utf-8",
        "apng" => "image/apng; charset=utf-8",
        "arc" => "application/x-freearc; charset=utf-8",
        "avif" => "image/avif; charset=utf-8",
        "avi" => "video/x-msvideo; charset=utf-8",
        "bin" => "application/octet-stream; charset=utf-8",
        "bmp" => "image/bmp; charset=utf-8",
        "bz" => "application/x-bzip; charset=utf-8",
        "bz2" => "application/x-bzip2; charset=utf-8",
        "cda" => "application/x-cdf; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "gz" => "application/gzip; charset=utf-8",
        "ico" => "image/vnd.microsoft.icon; charset=utf-8",
        "jsonld" => "application/ld+json; charset=utf-8",
        "mid" | "midi" => "audio/x-midi; charset=utf-8",
        "mp3" => "audio/mpeg; charset=utf-8",
        "mpeg" => "video/mpeg; charset=utf-8",
        "mpkg" => "application/vnd.apple.installer+xml; charset=utf-8",
        "ogg" | "oga" => "audio/ogg; charset=utf-8",
        "ogv" => "video/ogg; charset=utf-8",
        "ogx" => "application/ogg; charset=utf-8",
        "opus" => "audio/opus; charset=utf-8",
        "rar" => "application/vnd.rar; charset=utf-8",
        "sh" => "application/x-sh; charset=utf-8",
        "tar" => "application/x-tar; charset=utf-8",
        "tif" | "tiff" => "image/tiff; charset=utf-8",
        "ts" => "video/mp2t; charset=utf-8",
        "txt" | "osu" => "text/plain; charset=utf-8",
        "weba" => "audio/webm; charset=utf-8",
        "webm" => "video/webm; charset=utf-8",
        "webp" => "image/webp; charset=utf-8",
        "xml" => "application/xmlz; charset=utf-8",
        "xhtml" => "application/xhtml+xml; charset=utf-8",
        "xul" => "application/vnd.mozilla.xul+xml; charset=utf-8",
        "zip" => "application/zip; charset=utf-8",
        "3gp" => "video/3gpp;; charset=utf-8",
        "3g2" => "video/3gpp2;; charset=utf-8",
        "7z" => "application/x-7z-compressed; charset=utf-8",
        "swf" => "application/x-shockwave-flash; charset=utf-8",
        _ => "",
    }
}
