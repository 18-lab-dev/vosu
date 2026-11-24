"""Closed normalization inventory for M4b behavioral goldens.

`tests/fixtures/upstream/GOLDEN_CONTRACT.md` §7 is normative. Every rule here
has an exact scope; nothing matches a key, header or value by pattern. Adding
or widening a rule needs a contract amendment, a counterexample test and a
`NORMALIZATION_VERSION` bump.
"""
from __future__ import annotations

from dataclasses import dataclass
import json
import re
from typing import Callable
from urllib.parse import quote, unquote

NORMALIZATION_VERSION = 1

RULE_IDS = (
    "header-date",
    "header-last-modified",
    "header-etag",
    "root-prefix",
    "envelope-identity-ordinal",
    "raw-capability-redaction",
    "client-timestamp-pointer",
)

# (case family, record part, JSON Pointer) scopes approved for
# client-timestamp-pointer. Version 1 approves none.
CLIENT_TIMESTAMP_POINTERS: tuple[tuple[str, str, str], ...] = ()

DATE_PLACEHOLDER = "<golden:date>"
LAST_MODIFIED_PLACEHOLDER = "<golden:last-modified>"
CLIENT_TIMESTAMP_PLACEHOLDER = "<golden:client-timestamp>"
CAPABILITY_HEADER = "x-vosu-test-capture"
CREDENTIAL_HEADERS = frozenset({"authorization", "proxy-authorization", "cookie", "set-cookie"})
VALIDATOR_REQUEST_HEADERS = frozenset({
    "if-none-match", "if-range", "if-modified-since", "if-unmodified-since"})
OLD_BRAND = re.compile(r"tosu", re.IGNORECASE)

_IMF_FIXDATE = re.compile(
    r"(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} "
    r"(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{4} \d{2}:\d{2}:\d{2} GMT")
_ENTITY_TAG = re.compile(r'(W/)?"([\x21\x23-\x7e\x80-\xff]*)"')


class GoldenError(ValueError):
    """A capture or candidate violates the golden contract. Messages never echo private data."""


def approved_rule_ids(contract_text: str) -> tuple[int, set[str]]:
    """Return the contract's normalization version and its `| Rule ID |` table's first column."""
    version = re.search(r"(?im)^\W*normalization version:\W*(\d+)\W*$", contract_text)
    if version is None:
        raise GoldenError("approved rules: no 'Normalization version:' line")
    ids: set[str] = set()
    in_table = False
    for line in contract_text.splitlines():
        stripped = line.strip()
        if re.match(r"^\|\s*Rule ID\s*\|", stripped):
            in_table = True
            continue
        if not in_table:
            continue
        if not stripped.startswith("|"):
            break
        first = stripped.strip("|").split("|", 1)[0].strip()
        if set(first) <= set("-: "):
            continue
        match = re.fullmatch(r"`([a-z0-9-]+)`", first)
        if match is None:
            raise GoldenError(f"approved rules: malformed rule cell {first!r}")
        if match.group(1) in ids:
            raise GoldenError(f"approved rules: duplicate rule {match.group(1)!r}")
        ids.add(match.group(1))
    if not ids:
        raise GoldenError("approved rules: no '| Rule ID |' table")
    return int(version.group(1)), ids


def check_approved_rules(contract_text: str) -> None:
    version, ids = approved_rule_ids(contract_text)
    if version != NORMALIZATION_VERSION:
        raise GoldenError(
            f"approved rules: contract normalization version {version} differs from "
            f"implemented version {NORMALIZATION_VERSION}")
    implemented = set(RULE_IDS)
    if ids != implemented:
        raise GoldenError(
            "approved rules: implemented rules differ from the contract table "
            f"(unapproved {sorted(implemented - ids)}, unimplemented {sorted(ids - implemented)})")


# ---------------------------------------------------------------- headers


def is_imf_fixdate(value: str) -> bool:
    return _IMF_FIXDATE.fullmatch(value) is not None


def entity_tag(value: str) -> tuple[str, str] | None:
    match = _ENTITY_TAG.fullmatch(value)
    return None if match is None else (match.group(1) or "", match.group(2))


class Validators:
    """Platform-wide `header-etag` ordinals and observed `header-last-modified` values."""

    def __init__(self) -> None:
        self.etags: dict[str, int] = {}
        self.modified: set[str] = set()

    def observe(self, headers: list[list[str]]) -> None:
        for name, value in headers:
            lowered = name.lower()
            if lowered == "etag":
                tag = entity_tag(value)
                if tag is None:
                    raise GoldenError("header-etag invariant: response ETag is not a valid entity-tag")
                self.etags.setdefault(tag[1], len(self.etags) + 1)
            elif lowered == "last-modified":
                if not is_imf_fixdate(value):
                    raise GoldenError("header-last-modified invariant: Last-Modified is not an IMF-fixdate")
                self.modified.add(value)

    def etag(self, value: str) -> str:
        weak, opaque = entity_tag(value) or ("", "")
        return f'{weak}"<golden:etag:{self.etags[opaque]}>"'

    def request_value(self, name: str, value: str) -> tuple[str, set[str]]:
        """Replace echoed validators in one conditional request header value."""
        used: set[str] = set()
        lowered = name.lower()
        if lowered in ("if-modified-since", "if-unmodified-since", "if-range") and value in self.modified:
            return LAST_MODIFIED_PLACEHOLDER, {"header-last-modified"}
        if lowered not in ("if-none-match", "if-range"):
            return value, used

        def replace(match: re.Match[str]) -> str:
            if match.group(2) not in self.etags:
                return match.group(0)
            used.add("header-etag")
            return f'{match.group(1) or ""}"<golden:etag:{self.etags[match.group(2)]}>"'

        return _ENTITY_TAG.sub(replace, value), used


def normalize_response_headers(
        headers: list[list[str]], files_family: bool, validators: Validators,
) -> tuple[list[list[str]], set[str]]:
    out: list[list[str]] = []
    used: set[str] = set()
    for name, value in headers:
        lowered = name.lower()
        if lowered == "date":
            if not is_imf_fixdate(value):
                raise GoldenError("header-date invariant: Date is not an IMF-fixdate")
            value = DATE_PLACEHOLDER
            used.add("header-date")
        elif files_family and lowered == "etag":
            value = validators.etag(value)
            used.add("header-etag")
        elif files_family and lowered == "last-modified":
            value = LAST_MODIFIED_PLACEHOLDER
            used.add("header-last-modified")
        out.append([name, value])
    return out, used


# ---------------------------------------------------------------- roots

_SLOT = re.compile(r"[A-Za-z0-9!_-][A-Za-z0-9!._-]{0,63}")
_PATH_CHAR_BEFORE = r"(?<![A-Za-z0-9_.!~$\\/%-])"
_BARE_AFTER = r"(?=$|[\"'\\/<>|;,&)\]}\r\n])"


def _json1(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)[1:-1]


# Earlier encoders win when two produce the same private spelling (for example a
# forward-slash root that percent-encoding leaves unchanged).
_ENCODERS: tuple[Callable[[str], str], ...] = (
    lambda value: value,
    _json1,
    lambda value: json.dumps(value)[1:-1],
    lambda value: _json1(_json1(value)),
    lambda value: quote(value, safe="!~*'()"),
    lambda value: quote(value, safe=""),
    lambda value: quote(value, safe="/"),
    lambda value: quote(value, safe="/:"),
)


@dataclass(frozen=True)
class Root:
    slot: str
    private: str
    synthetic: str


def synthetic_root(slot: str, windows: bool) -> str:
    if _SLOT.fullmatch(slot) is None or slot in (".", ".."):
        raise GoldenError(f"root slot {slot!r} is not a valid synthetic root name")
    return f"C:\\vosu-golden\\{slot}\\" if windows else f"/vosu-golden/{slot}/"


def validate_private_root(slot: str, private: str, windows: bool) -> None:
    if windows:
        shape = re.fullmatch(r"[A-Za-z]:\\(?:[^\\/:*?\"<>|\r\n]+\\)+", private)
    else:
        shape = re.fullmatch(r"/(?:[^/\0\r\n]+/){2,}", private)
    if shape is None:
        raise GoldenError(
            f"root {slot!r}: private root is not an absolute "
            f"{'Windows drive' if windows else 'POSIX'} directory prefix ending in a separator "
            f"{'' if windows else 'with at least two segments '}")
    if "vosu-golden" in private.lower():
        raise GoldenError(f"root {slot!r}: private root overlaps the synthetic root namespace")


def _bases(root: Root, windows: bool) -> list[tuple[str, str]]:
    private, synthetic = root.private, root.synthetic
    bases = [(private, synthetic)]
    if windows:
        bases.append((private.replace("\\", "/"), synthetic.replace("\\", "/")))
        for prefix in ("\\\\?\\", "\\\\.\\"):
            bases.append((prefix + private, prefix + synthetic))
        for host in ("localhost", "127.0.0.1"):
            bases.append((f"\\\\{host}\\{private[0]}$\\{private[3:]}",
                          f"\\\\{host}\\C$\\{synthetic[3:]}"))
    return bases


class RootRewriter:
    """`root-prefix`: attested private prefixes to synthetic roots at exact token boundaries."""

    def __init__(self, roots: list[Root], windows: bool) -> None:
        self.roots = roots
        lowered = [root.private.lower() for root in roots]
        for index, outer in enumerate(lowered):
            for other, inner in enumerate(lowered):
                if index != other and inner.startswith(outer):
                    raise GoldenError(
                        f"roots {roots[index].slot!r} and {roots[other].slot!r} are nested; "
                        "declare only the outer root")
        if len({root.synthetic for root in roots}) != len(roots):
            raise GoldenError("two attested roots map to the same synthetic root")
        replacements: dict[str, tuple[str, str]] = {}
        self._private_forms: list[tuple[str, str]] = []
        synthetic_forms: set[str] = set()
        for root in roots:
            for private, synthetic in _bases(root, windows):
                for encode in _ENCODERS:
                    for p_form, s_form, bare in (
                            (encode(private), encode(synthetic), False),
                            (encode(private[:-1]), encode(synthetic[:-1]), True)):
                        key = p_form + ("\0bare" if bare else "")
                        if replacements.setdefault(key, (root.slot, s_form))[0] != root.slot:
                            raise GoldenError("ambiguous root spelling across attested roots")
                        self._private_forms.append((root.slot, p_form.lower()))
                        synthetic_forms.add(s_form)
        ordered = sorted(replacements, key=lambda key: (-len(key), key))
        self._lookup = {key.removesuffix("\0bare"): replacements[key][1] for key in ordered}
        alternatives = [
            _PATH_CHAR_BEFORE + re.escape(key.removesuffix("\0bare"))
            + (_BARE_AFTER if key.endswith("\0bare") else "")
            for key in ordered
        ]
        self._pattern = re.compile("|".join(alternatives)) if alternatives else None
        self._synthetic = sorted(synthetic_forms, key=lambda form: (-len(form), form))

    def text(self, value: str) -> tuple[str, int]:
        if self._pattern is None:
            return value, 0
        return self._pattern.subn(lambda match: self._lookup[match.group(0)], value)

    def without_synthetic(self, value: str) -> str:
        for form in self._synthetic:
            value = value.replace(form, "\0")
        return value

    def leftover_slots(self, value: str) -> list[str]:
        haystack = self.without_synthetic(value).lower()
        views = (haystack, unquote(haystack))
        return sorted({slot for slot, form in self._private_forms
                       if form and any(form in view for view in views)})


# Generic private-path detectors run after synthetic roots are removed.
_GENERIC_PRIVATE = (
    ("drive path", re.compile(r"(?<![A-Za-z0-9])[A-Za-z]:(?:\\|/)")),
    ("admin share", re.compile(r"(?<![A-Za-z0-9])[A-Za-z]\$(?:\\|/)")),
    ("device path", re.compile(r"(?:\\){2,}[?.](?:\\)+")),
    ("profile directory", re.compile(
        r"(?i)(?:\\|/)(?:users|home|documents and settings|appdata)(?:\\|/)")),
    ("private POSIX directory", re.compile(
        r"(?i)(?<![A-Za-z0-9._~-])/(?:root|tmp|mnt|media|run/user|var/folders|private/var)/")),
)


def privacy_findings(text: str, rewriter: RootRewriter) -> list[str]:
    """Return check names (never content) that found private data in `text`."""
    findings = [f"unnormalized private root {slot!r}" for slot in rewriter.leftover_slots(text)]
    stripped = rewriter.without_synthetic(text)
    for view in (stripped, unquote(stripped)):
        for name, pattern in _GENERIC_PRIVATE:
            if pattern.search(view) and f"private path ({name})" not in findings:
                findings.append(f"private path ({name})")
        if CAPABILITY_HEADER in view.lower() and "capability header" not in findings:
            findings.append("capability header")
    return findings


# ---------------------------------------------------------------- client timestamps

_JSON_WS = re.compile(r"[ \t\n\r]*")
_JSON_NUMBER = re.compile(r"-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?")
_MAX_JSON_DEPTH = 128


def json_value_spans(text: str) -> dict[str, tuple[int, int, str] | None]:
    """Map every JSON Pointer to its raw `(start, end, kind)` span without reserializing.

    A pointer reached through a duplicate object key maps to None (ambiguous).
    """
    spans: dict[str, tuple[int, int, str] | None] = {}

    def skip(pos: int) -> int:
        return _JSON_WS.match(text, pos).end()

    def record(pointer: str, span: tuple[int, int, str]) -> None:
        spans[pointer] = None if pointer in spans else span

    def value(pos: int, pointer: str, depth: int) -> int:
        if depth > _MAX_JSON_DEPTH:
            raise ValueError("JSON nesting too deep")
        pos = skip(pos)
        start = pos
        char = text[pos:pos + 1]
        if char == '"':
            _, end = json.decoder.scanstring(text, pos + 1, True)
            record(pointer, (start, end, "string"))
            return end
        if char in ("{", "["):
            closing = "}" if char == "{" else "]"
            pos = skip(pos + 1)
            index = 0
            if text[pos:pos + 1] != closing:
                while True:
                    if char == "{":
                        if text[pos:pos + 1] != '"':
                            raise ValueError("expected object key")
                        key, pos = json.decoder.scanstring(text, pos + 1, True)
                        pos = skip(pos)
                        if text[pos:pos + 1] != ":":
                            raise ValueError("expected ':'")
                        child = pointer + "/" + key.replace("~", "~0").replace("/", "~1")
                        pos = skip(value(pos + 1, child, depth + 1))
                    else:
                        pos = skip(value(pos, f"{pointer}/{index}", depth + 1))
                    index += 1
                    if text[pos:pos + 1] == ",":
                        pos = skip(pos + 1)
                        continue
                    break
            if text[pos:pos + 1] != closing:
                raise ValueError("unterminated container")
            record(pointer, (start, pos + 1, "object" if char == "{" else "array"))
            return pos + 1
        for literal, kind in (("true", "boolean"), ("false", "boolean"), ("null", "null")):
            if text.startswith(literal, pos):
                record(pointer, (start, pos + len(literal), kind))
                return pos + len(literal)
        number = _JSON_NUMBER.match(text, pos)
        if number is None or number.end() == pos:
            raise ValueError("invalid JSON value")
        record(pointer, (start, number.end(), "number"))
        return number.end()

    end = skip(value(0, "", 0))
    if end != len(text):
        raise ValueError("trailing data after JSON value")
    return spans


def apply_timestamp_pointers(
        text: str, family: str, part: str,
        scopes: tuple[tuple[str, str, str], ...] = CLIENT_TIMESTAMP_POINTERS,
) -> tuple[str, bool]:
    """`client-timestamp-pointer`: replace string values at approved exact scopes only."""
    pointers = [pointer for scope_family, scope_part, pointer in scopes
                if scope_family == family and scope_part == part]
    if not pointers:
        return text, False
    try:
        spans = json_value_spans(text)
    except ValueError:
        return text, False
    edits: list[tuple[int, int]] = []
    for pointer in pointers:
        if pointer not in spans:
            continue
        span = spans[pointer]
        if span is None:
            raise GoldenError(f"client-timestamp-pointer {pointer!r} is ambiguous (duplicate key)")
        if span[2] != "string":
            raise GoldenError(
                f"client-timestamp-pointer {pointer!r} targets a {span[2]}; numbers, booleans, "
                "null and containers are never normalized")
        edits.append(span[:2])
    for start, end in sorted(edits, reverse=True):
        text = text[:start] + json.dumps(CLIENT_TIMESTAMP_PLACEHOLDER) + text[end:]
    return text, bool(edits)


# ---------------------------------------------------------------- CBOR text strings

_MAX_CBOR_DEPTH = 64


def _cbor_head(major: int, length: int) -> bytes:
    if length < 24:
        return bytes([major << 5 | length])
    for info, size in ((24, 1), (25, 2), (26, 4), (27, 8)):
        if length < 1 << (8 * size):
            return bytes([major << 5 | info]) + length.to_bytes(size, "big")
    raise GoldenError("CBOR length out of range")


def rewrite_cbor_text(
        data: bytes, rewrite: Callable[[str], str],
) -> tuple[bytes, list[str], list[bytes]]:
    """Rewrite definite-length CBOR text strings; copy every other byte verbatim.

    Returns the new bytes, every (rewritten) text string and every byte string,
    so callers can scan content that base64 would otherwise hide.
    """
    out = bytearray()
    texts: list[str] = []
    blobs: list[bytes] = []

    def head(pos: int) -> tuple[int, int, int]:
        if pos >= len(data):
            raise GoldenError("truncated CBOR item")
        initial = data[pos]
        major, info = initial >> 5, initial & 31
        if info < 24:
            return major, info, pos + 1
        if info in (24, 25, 26, 27):
            size = 1 << (info - 24)
            if pos + 1 + size > len(data):
                raise GoldenError("truncated CBOR head")
            return major, int.from_bytes(data[pos + 1:pos + 1 + size], "big"), pos + 1 + size
        raise GoldenError("non-canonical CBOR (indefinite length or reserved additional info)")

    def item(pos: int, depth: int) -> tuple[int, bool]:
        if depth > _MAX_CBOR_DEPTH:
            raise GoldenError("CBOR nesting too deep")
        major, argument, body = head(pos)
        if major in (2, 3):
            end = body + argument
            if end > len(data):
                raise GoldenError("truncated CBOR string")
            raw = data[body:end]
            if major == 2:
                blobs.append(raw)
                out.extend(data[pos:end])
                return end, False
            try:
                text = raw.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise GoldenError("CBOR text string is not valid UTF-8") from exc
            changed = rewrite(text)
            texts.append(changed)
            if changed == text:
                out.extend(data[pos:end])
                return end, False
            encoded = changed.encode("utf-8")
            out.extend(_cbor_head(3, len(encoded)) + encoded)
            return end, True
        out.extend(data[pos:body])
        if major in (0, 1, 7):
            return body, False
        if major == 6:
            return item(body, depth + 1)
        count = argument * (2 if major == 5 else 1)
        if count > len(data) - body:
            raise GoldenError("CBOR container longer than its data")
        changed = False
        for index in range(count):
            body, item_changed = item(body, depth + 1)
            if major == 5 and index % 2 == 0 and item_changed:
                raise GoldenError("root-prefix would change a CBOR map key (canonical order)")
            changed |= item_changed
        return body, changed

    end, _ = item(0, 0)
    if end != len(data):
        raise GoldenError("trailing bytes after CBOR item")
    return bytes(out), texts, blobs
