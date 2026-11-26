//! Pins every opcode, enum value, flag bit, and buffer-size constant
//! `vosu-plugin-api` defines to the literal values `include/vosu_plugin.h` declares
//! (minor 2): parses the header's own `typedef enum`/`#define` text at test
//! time, rather than trusting that a hand check made once stays true, so a
//! header edit that disagrees with the SDK's Rust constants fails here
//! even if nobody touches its `tests/layout.rs` struct-layout assertions.
//! Workspace-only (the header is not in the SDK package, AF.9).

use std::collections::HashMap;

use vosu_plugin_api::{
    VOSU_ARTIFACT_NAME_MAX_BYTES, VOSU_FRAME_MAX_BYTES, VOSU_OPCODE_PRIVATE_BIT,
    VOSU_PLUGIN_ABI_MAJOR, VOSU_PUBLISH_BATCH_MAX_BYTES, VOSU_READ_PLAN_MAX_BYTES,
    VOSU_READ_PLAN_SYNC, VOSU_STREAM_CHUNK_MAX_BYTES, vosu_opcode, vosu_plan_base_kind,
    vosu_read_error_code, vosu_read_result_kind, vosu_stage_base_kind, vosu_status,
    vosu_value_kind,
};

/// Strips every `/* ... */` block comment, replacing it with a single
/// space so an identifier straddling the removed text never fuses with
/// its neighbor.
fn strip_block_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next(); // consume '*'
            let mut prev = '\0';
            for c in chars.by_ref() {
                if prev == '*' && c == '/' {
                    break;
                }
                prev = c;
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses every `typedef enum <name> { ... } <name>;` block into an
/// ordered list of `(variant, value)` pairs, keyed by enum name.
fn parse_enums(source: &str) -> HashMap<String, Vec<(String, u32)>> {
    let mut enums = HashMap::new();
    let mut rest = source;
    while let Some(start) = rest.find("typedef enum ") {
        let after_kw = &rest[start + "typedef enum ".len()..];
        let brace = after_kw.find('{').expect("enum missing opening brace");
        let name = after_kw[..brace].trim().to_owned();
        let after_brace = &after_kw[brace + 1..];
        let close = after_brace.find('}').expect("enum missing closing brace");
        let body = &after_brace[..close];

        let mut variants = Vec::new();
        for entry in body.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (variant, value) = entry.split_once('=').unwrap_or_else(|| {
                panic!("enum {name} variant `{entry}` has no explicit `= <value>`")
            });
            let value: u32 = value.trim().parse().unwrap_or_else(|error| {
                panic!("enum {name} variant `{entry}`'s value did not parse: {error}")
            });
            variants.push((variant.trim().to_owned(), value));
        }
        enums.insert(name, variants);
        rest = &after_brace[close + 1..];
    }
    enums
}

/// Tiny recursive-descent evaluator for the small subset of C constant
/// expressions `include/vosu_plugin.h`'s `#define`s use: unsigned integer
/// literals (optional `u`/`U` suffix), `+`, `-`, `*`, `<<`, parentheses,
/// and references to an already-defined macro.
struct ExprParser<'a> {
    tokens: Vec<&'a str>,
    pos: usize,
    macros: &'a HashMap<String, u32>,
}

impl<'a> ExprParser<'a> {
    fn new(expr: &'a str, macros: &'a HashMap<String, u32>) -> Self {
        let mut tokens = Vec::new();
        let mut rest = expr;
        while !rest.is_empty() {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let (token, remainder) = if let Some(stripped) = rest.strip_prefix("<<") {
                ("<<", stripped)
            } else if let Some(first) = rest.chars().next() {
                if "()+-*".contains(first) {
                    (&rest[..first.len_utf8()], &rest[first.len_utf8()..])
                } else {
                    let end = rest
                        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len());
                    (&rest[..end], &rest[end..])
                }
            } else {
                break;
            };
            tokens.push(token);
            rest = remainder;
        }
        ExprParser {
            tokens,
            pos: 0,
            macros,
        }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).copied()
    }

    fn next(&mut self) -> &'a str {
        let token = self.tokens[self.pos];
        self.pos += 1;
        token
    }

    fn parse_expr(&mut self) -> u32 {
        let mut value = self.parse_term();
        loop {
            match self.peek() {
                Some("+") => {
                    self.next();
                    value += self.parse_term();
                }
                Some("-") => {
                    self.next();
                    value -= self.parse_term();
                }
                Some("<<") => {
                    self.next();
                    value <<= self.parse_term();
                }
                _ => break,
            }
        }
        value
    }

    fn parse_term(&mut self) -> u32 {
        let mut value = self.parse_atom();
        while self.peek() == Some("*") {
            self.next();
            value *= self.parse_atom();
        }
        value
    }

    fn parse_atom(&mut self) -> u32 {
        let token = self.next();
        if token == "(" {
            let value = self.parse_expr();
            assert_eq!(self.next(), ")", "unbalanced parentheses in expression");
            return value;
        }
        let stripped = token.trim_end_matches(['u', 'U']);
        if let Ok(literal) = stripped.parse::<u32>() {
            return literal;
        }
        *self
            .macros
            .get(token)
            .unwrap_or_else(|| panic!("expression references unknown macro `{token}`"))
    }
}

fn eval_expr(expr: &str, macros: &HashMap<String, u32>) -> u32 {
    let mut parser = ExprParser::new(expr, macros);
    let value = parser.parse_expr();
    assert_eq!(
        parser.pos,
        parser.tokens.len(),
        "trailing tokens in `{expr}`"
    );
    value
}

/// Parses every `#define NAME <expr>` line into `name -> value`, resolving
/// each macro in file order so a later one (`VOSU_STREAM_CHUNK_MAX_BYTES`)
/// can reference an earlier one (`VOSU_FRAME_MAX_BYTES`).
fn parse_defines(source: &str) -> HashMap<String, u32> {
    let mut macros = HashMap::new();
    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("#define ") else {
            continue;
        };
        // Skip a value-less `#define` (an include guard like
        // `#define VOSU_PLUGIN_H`): nothing here needs it, and it has no
        // expression to evaluate.
        let Some((name, expr)) = rest.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let value = eval_expr(expr.trim(), &macros);
        macros.insert(name.to_owned(), value);
    }
    macros
}

fn header_source() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest_dir}/include/vosu_plugin.h");
    strip_block_comments(&std::fs::read_to_string(&path).expect("read include/vosu_plugin.h"))
}

fn assert_enum_matches(
    enums: &HashMap<String, Vec<(String, u32)>>,
    name: &str,
    rust: &[(&str, u32)],
) {
    let header = enums
        .get(name)
        .unwrap_or_else(|| panic!("include/vosu_plugin.h has no `{name}` enum"));
    let header: Vec<(String, u32)> = header.clone();
    let rust: Vec<(String, u32)> = rust.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect();
    assert_eq!(
        header, rust,
        "{name}: include/vosu_plugin.h and the Rust enum disagree"
    );
}

#[test]
fn opcode_values_match_the_header() {
    let source = header_source();
    let enums = parse_enums(&source);
    use vosu_opcode::*;
    assert_enum_matches(
        &enums,
        "vosu_opcode",
        &[
            ("VOSU_OP_PUBLISH_FIELD", VOSU_OP_PUBLISH_FIELD as u32),
            ("VOSU_OP_DEMAND_CHANGED", VOSU_OP_DEMAND_CHANGED as u32),
            ("VOSU_OP_READ_PLAN_SUBMIT", VOSU_OP_READ_PLAN_SUBMIT as u32),
            ("VOSU_OP_READ_PLAN_RESULT", VOSU_OP_READ_PLAN_RESULT as u32),
            ("VOSU_OP_READ_PLAN_CANCEL", VOSU_OP_READ_PLAN_CANCEL as u32),
            ("VOSU_OP_PUBLISH_BATCH", VOSU_OP_PUBLISH_BATCH as u32),
            ("VOSU_OP_SESSION_ATTACHED", VOSU_OP_SESSION_ATTACHED as u32),
            ("VOSU_OP_SESSION_DETACHED", VOSU_OP_SESSION_DETACHED as u32),
            ("VOSU_OP_SESSION_IDENTIFY", VOSU_OP_SESSION_IDENTIFY as u32),
            ("VOSU_OP_FOCUS_CHANGED", VOSU_OP_FOCUS_CHANGED as u32),
            ("VOSU_OP_SESSION_DECLINE", VOSU_OP_SESSION_DECLINE as u32),
            ("VOSU_OP_SCAN_REQUEST", VOSU_OP_SCAN_REQUEST as u32),
            ("VOSU_OP_SCAN_RESULT", VOSU_OP_SCAN_RESULT as u32),
            ("VOSU_OP_RESOURCE_RESOLVE", VOSU_OP_RESOURCE_RESOLVE as u32),
            (
                "VOSU_OP_RESOURCE_RESOLVED",
                VOSU_OP_RESOURCE_RESOLVED as u32,
            ),
            ("VOSU_OP_TREE_LIST", VOSU_OP_TREE_LIST as u32),
            ("VOSU_OP_TREE_LOOKUP", VOSU_OP_TREE_LOOKUP as u32),
            ("VOSU_OP_STREAM_READ", VOSU_OP_STREAM_READ as u32),
            ("VOSU_OP_STREAM_CHUNK", VOSU_OP_STREAM_CHUNK as u32),
            ("VOSU_OP_ROOT_DECLARE", VOSU_OP_ROOT_DECLARE as u32),
            ("VOSU_OP_STREAM_CLOSE", VOSU_OP_STREAM_CLOSE as u32),
            ("VOSU_OP_ACTION_INVOKE", VOSU_OP_ACTION_INVOKE as u32),
            ("VOSU_OP_ACTION_RESULT", VOSU_OP_ACTION_RESULT as u32),
            ("VOSU_OP_ACTION_CANCEL", VOSU_OP_ACTION_CANCEL as u32),
            ("VOSU_OP_ACTION_PROGRESS", VOSU_OP_ACTION_PROGRESS as u32),
            ("VOSU_OP_ACTION_CALL", VOSU_OP_ACTION_CALL as u32),
            ("VOSU_OP_SETTINGS_CHANGED", VOSU_OP_SETTINGS_CHANGED as u32),
            ("VOSU_OP_LOG_RECORD", VOSU_OP_LOG_RECORD as u32),
            ("VOSU_OP_TIMER_SET", VOSU_OP_TIMER_SET as u32),
            ("VOSU_OP_TIMER_CANCEL", VOSU_OP_TIMER_CANCEL as u32),
            ("VOSU_OP_TIMER_FIRED", VOSU_OP_TIMER_FIRED as u32),
            ("VOSU_OP_STATE_SUBSCRIBE", VOSU_OP_STATE_SUBSCRIBE as u32),
            ("VOSU_OP_STATE_UPDATE", VOSU_OP_STATE_UPDATE as u32),
            ("VOSU_OP_ARTIFACT_DECLARE", VOSU_OP_ARTIFACT_DECLARE as u32),
            (
                "VOSU_OP_ARTIFACT_DECLARED",
                VOSU_OP_ARTIFACT_DECLARED as u32,
            ),
            ("VOSU_OP_ARTIFACT_OPEN", VOSU_OP_ARTIFACT_OPEN as u32),
            ("VOSU_OP_ARTIFACT_RELEASE", VOSU_OP_ARTIFACT_RELEASE as u32),
        ],
    );
}

#[test]
fn status_values_match_the_header() {
    let source = header_source();
    let enums = parse_enums(&source);
    use vosu_status::*;
    assert_enum_matches(
        &enums,
        "vosu_status",
        &[
            ("VOSU_OK", VOSU_OK as u32),
            ("VOSU_ERR_UNSUPPORTED", VOSU_ERR_UNSUPPORTED as u32),
            (
                "VOSU_ERR_INVALID_ARGUMENT",
                VOSU_ERR_INVALID_ARGUMENT as u32,
            ),
            ("VOSU_ERR_NOT_READY", VOSU_ERR_NOT_READY as u32),
            (
                "VOSU_ERR_PERMISSION_DENIED",
                VOSU_ERR_PERMISSION_DENIED as u32,
            ),
            ("VOSU_ERR_TARGET_GONE", VOSU_ERR_TARGET_GONE as u32),
            ("VOSU_ERR_BACKEND", VOSU_ERR_BACKEND as u32),
            ("VOSU_ERR_CANCELLED", VOSU_ERR_CANCELLED as u32),
            ("VOSU_ERR_TIMED_OUT", VOSU_ERR_TIMED_OUT as u32),
        ],
    );
}

#[test]
fn value_kind_values_match_the_header() {
    let source = header_source();
    let enums = parse_enums(&source);
    use vosu_value_kind::*;
    assert_enum_matches(
        &enums,
        "vosu_value_kind",
        &[
            ("VOSU_VALUE_NULL", VOSU_VALUE_NULL as u32),
            ("VOSU_VALUE_BOOL", VOSU_VALUE_BOOL as u32),
            ("VOSU_VALUE_I64", VOSU_VALUE_I64 as u32),
            ("VOSU_VALUE_U64", VOSU_VALUE_U64 as u32),
            ("VOSU_VALUE_F64", VOSU_VALUE_F64 as u32),
            ("VOSU_VALUE_STRING", VOSU_VALUE_STRING as u32),
            ("VOSU_VALUE_BYTES", VOSU_VALUE_BYTES as u32),
            ("VOSU_VALUE_CBOR", VOSU_VALUE_CBOR as u32),
            ("VOSU_VALUE_RESOURCE_REF", VOSU_VALUE_RESOURCE_REF as u32),
            ("VOSU_VALUE_TREE_REF", VOSU_VALUE_TREE_REF as u32),
        ],
    );
}

#[test]
fn read_plan_enum_values_match_the_header() {
    let source = header_source();
    let enums = parse_enums(&source);

    use vosu_plan_base_kind::*;
    assert_enum_matches(
        &enums,
        "vosu_plan_base_kind",
        &[
            ("VOSU_BASE_ABSOLUTE", VOSU_BASE_ABSOLUTE as u32),
            ("VOSU_BASE_MODULE", VOSU_BASE_MODULE as u32),
        ],
    );

    use vosu_stage_base_kind::*;
    assert_enum_matches(
        &enums,
        "vosu_stage_base_kind",
        &[
            ("VOSU_STAGE_BASE_ROOT", VOSU_STAGE_BASE_ROOT as u32),
            (
                "VOSU_STAGE_BASE_DEPENDENT",
                VOSU_STAGE_BASE_DEPENDENT as u32,
            ),
        ],
    );

    use vosu_read_error_code::*;
    assert_enum_matches(
        &enums,
        "vosu_read_error_code",
        &[
            ("VOSU_READ_ERROR_BACKEND", VOSU_READ_ERROR_BACKEND as u32),
            (
                "VOSU_READ_ERROR_PERMISSION_DENIED",
                VOSU_READ_ERROR_PERMISSION_DENIED as u32,
            ),
            (
                "VOSU_READ_ERROR_READ_FAILED",
                VOSU_READ_ERROR_READ_FAILED as u32,
            ),
            (
                "VOSU_READ_ERROR_ADDRESS_OVERFLOW",
                VOSU_READ_ERROR_ADDRESS_OVERFLOW as u32,
            ),
        ],
    );

    use vosu_read_result_kind::*;
    assert_enum_matches(
        &enums,
        "vosu_read_result_kind",
        &[
            ("VOSU_READ_OK", VOSU_READ_OK as u32),
            ("VOSU_READ_NOT_READY", VOSU_READ_NOT_READY as u32),
            ("VOSU_READ_UNSUPPORTED", VOSU_READ_UNSUPPORTED as u32),
            ("VOSU_READ_ERROR", VOSU_READ_ERROR as u32),
        ],
    );
}

#[test]
fn buffer_size_and_flag_constants_match_the_header() {
    let source = header_source();
    let macros = parse_defines(&source);

    let expected: &[(&str, u32)] = &[
        ("VOSU_PLUGIN_ABI_MAJOR", VOSU_PLUGIN_ABI_MAJOR),
        ("VOSU_READ_PLAN_MAX_BYTES", VOSU_READ_PLAN_MAX_BYTES),
        ("VOSU_FRAME_MAX_BYTES", VOSU_FRAME_MAX_BYTES),
        ("VOSU_PUBLISH_BATCH_MAX_BYTES", VOSU_PUBLISH_BATCH_MAX_BYTES),
        ("VOSU_STREAM_CHUNK_MAX_BYTES", VOSU_STREAM_CHUNK_MAX_BYTES),
        ("VOSU_ARTIFACT_NAME_MAX_BYTES", VOSU_ARTIFACT_NAME_MAX_BYTES),
        ("VOSU_READ_PLAN_SYNC", VOSU_READ_PLAN_SYNC),
        ("VOSU_OPCODE_PRIVATE_BIT", VOSU_OPCODE_PRIVATE_BIT),
    ];
    for (name, rust_value) in expected {
        let header_value = macros
            .get(*name)
            .unwrap_or_else(|| panic!("include/vosu_plugin.h has no `#define {name}`"));
        assert_eq!(
            header_value, rust_value,
            "{name}: include/vosu_plugin.h and the Rust constant disagree"
        );
    }
}

/// The A5.2 reservation: the private bit is exactly the high bit, no
/// header opcode sets it, and the SDK refuses every opcode carrying it.
#[test]
fn private_opcode_bit_is_reserved_from_every_public_opcode() {
    assert_eq!(VOSU_OPCODE_PRIVATE_BIT, 0x8000_0000);
    let source = header_source();
    let enums = parse_enums(&source);
    let opcodes = &enums["vosu_opcode"];
    assert!(!opcodes.is_empty());
    for (name, value) in opcodes {
        assert_eq!(
            value & VOSU_OPCODE_PRIVATE_BIT,
            0,
            "{name} sets the reserved private bit"
        );
        assert!(vosu_opcode::try_from(*value).is_ok(), "{name}");
        assert!(
            vosu_opcode::try_from(*value | VOSU_OPCODE_PRIVATE_BIT).is_err(),
            "{name} with the private bit must not decode"
        );
    }
    for private in [0x8000_0000, 0x8000_0001, 0x8000_0012, u32::MAX] {
        assert!(vosu_opcode::try_from(private).is_err(), "{private:#x}");
    }
}
