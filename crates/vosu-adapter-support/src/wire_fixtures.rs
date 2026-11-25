//! Awkward protocol-independent values for byte-equivalence tests.

/// Strings that exercise every serde_json escape class and non-ASCII text.
pub const AWKWARD: &str = "q\"b\\s/\u{1}\u{8}\u{c}\n\r\t\u{1f}\u{7f}é\u{2028}🎵";

/// Numbers whose JavaScript spelling differs from naive formatting.
pub const NUMBERS: [f64; 12] = [
    -0.0,
    0.1,
    -2.5,
    1e-7,
    123_456.789,
    9_007_199_254_740_991.0,
    9_007_199_254_740_993.0,
    1e21,
    -1.5e300,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];
