//! Signature (AOB) pattern parsing and matching. The region walk that applies
//! a pattern to a target's readable memory is native (`vosu-sys`), reached by
//! a plugin only through `VOSU_OP_SCAN_REQUEST`.

use std::fmt;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    mask: Vec<u8>,
    non_zero_mask: bool,
    anchor_offset: usize,
    anchor_len: usize,
}

impl Pattern {
    /// Parses space-separated bytes. A token is either exactly two hexadecimal
    /// digits or the wildcard `??`.
    pub fn parse(value: &str, non_zero_mask: bool) -> Result<Self, PatternParseError> {
        let mut bytes = Vec::new();
        let mut mask = Vec::new();

        for (index, token) in value.split_whitespace().enumerate() {
            if token == "??" {
                bytes.push(0);
                mask.push(0);
                continue;
            }

            if token.len() != 2 {
                return Err(PatternParseError::InvalidToken {
                    index,
                    token: token.to_owned(),
                });
            }

            let byte =
                u8::from_str_radix(token, 16).map_err(|_| PatternParseError::InvalidToken {
                    index,
                    token: token.to_owned(),
                })?;
            bytes.push(byte);
            mask.push(1);
        }

        let (anchor_offset, anchor_len) = longest_fixed_run(&mask);
        Ok(Self {
            bytes,
            mask,
            non_zero_mask,
            anchor_offset,
            anchor_len,
        })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    #[must_use]
    pub const fn non_zero_mask(&self) -> bool {
        self.non_zero_mask
    }

    /// Offset and length of the longest run of fixed (non-wildcard) bytes,
    /// computed once at parse time so a scanner can search for that run with
    /// a substring finder before checking the whole pattern. A zero length
    /// means every byte is a wildcard.
    #[must_use]
    pub const fn fixed_anchor(&self) -> (usize, usize) {
        (self.anchor_offset, self.anchor_len)
    }

    /// Whether the whole pattern matches `window` starting at `offset`. A
    /// fixed byte must be equal; a wildcard accepts any byte, or only a
    /// non-zero one when [`Self::non_zero_mask`] is set. A pattern that would
    /// extend past the end of `window` does not match.
    #[must_use]
    pub fn matches_at(&self, window: &[u8], offset: usize) -> bool {
        let Some(candidate) = offset
            .checked_add(self.bytes.len())
            .and_then(|end| window.get(offset..end))
        else {
            return false;
        };
        candidate.iter().zip(self.bytes.iter().zip(&self.mask)).all(
            |(&actual, (&expected, &mask))| {
                if mask != 0 {
                    actual == expected
                } else if self.non_zero_mask {
                    actual != 0
                } else {
                    true
                }
            },
        )
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PatternParseError {
    #[error(
        "invalid signature token {token:?} at index {index}; expected two hexadecimal digits or ??"
    )]
    InvalidToken { index: usize, token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchScanResult {
    pub index: usize,
    pub address: usize,
}

fn longest_fixed_run(mask: &[u8]) -> (usize, usize) {
    let mut best = (0, 0);
    let mut start = 0;
    while start < mask.len() {
        while start < mask.len() && mask[start] == 0 {
            start += 1;
        }
        let mut end = start;
        while end < mask.len() && mask[end] != 0 {
            end += 1;
        }
        if end - start > best.1 {
            best = (start, end - start);
        }
        start = end;
    }
    best
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (&byte, &mask)) in self.bytes.iter().zip(&self.mask).enumerate() {
            if index != 0 {
                formatter.write_str(" ")?;
            }
            if mask == 0 {
                formatter.write_str("??")?;
            } else {
                write!(formatter, "{byte:02X}")?;
            }
        }
        Ok(())
    }
}
