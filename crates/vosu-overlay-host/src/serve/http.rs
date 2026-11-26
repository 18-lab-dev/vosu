//! Byte-range planning and media-range eligibility, identical to core's
//! file routes: one RFC 9110 range, multiple ranges rejected, an end beyond
//! EOF unsatisfiable rather than clamped (upstream compatibility). Media
//! types come from the shared `vosu_surface::files::content_type` table.

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
