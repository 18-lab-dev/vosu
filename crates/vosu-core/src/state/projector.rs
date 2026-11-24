//! The D2 projector: the single function family every API-facing or logged
//! view of a session document goes through (S13, E5; plan:585-588;
//! ss:297-339). `vosu.osu.internal` is stripped from an [`Audience::Api`]
//! view unconditionally, regardless of any plugin's declared capabilities
//! (ss:331-339). A [`Audience::Plugin`] view instead exposes exactly the
//! subtree(s) its [`InternalGrant`] names, and everything below them (MG) —
//! never a sibling, and never the rest of `internal` from a whole-namespace
//! `field:vosu.osu` requirement.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use super::document::SessionState;
use super::pointer::{Pointer, tokens_prefix};
use super::schema::SchemaRegistry;
use super::value::{StateObject, StateValue, write_document_json};

/// Who a projected view is for: the API v1 surface (`internal` always
/// absent), or a plugin reading through its own ABI grant.
pub enum Audience<'g> {
    Api,
    Plugin(&'g InternalGrant),
}

/// The subtree(s) below a namespace's `x-visibility: "plugins"` key
/// (`vosu.osu.internal` in this freeze) a plugin's manifest `requires`/
/// `optional` named (ss:342-349, MG). Naming a descendant grants exactly
/// that descendant, not its ancestor or siblings; naming the whole
/// namespace (`field:vosu.osu`) grants none of it.
#[derive(Clone, Default, Debug)]
pub struct InternalGrant {
    subtrees: Vec<Pointer>,
}

impl InternalGrant {
    /// No subtree granted: every `internal` descendant reads as absent.
    pub fn none() -> Self {
        Self::default()
    }

    /// Builds a grant from already-resolved internal-subtree pointers (a
    /// plugin's `requires`/`optional` entries naming `field:vosu.osu.internal`
    /// or a descendant, namespace-stripped to the pointer below it).
    pub fn from_subtrees(subtrees: Vec<Pointer>) -> Self {
        Self { subtrees }
    }

    /// Resolves a grant from a plugin's manifest. `docs/v2/catalog-v2.md`'s
    /// `[[requires]]`/`[[optional]]` capability-id parsing is K0's scope;
    /// `vosu_plugin_api::PluginManifest` does not carry those fields in
    /// this freeze (`crates/vosu-plugin-api/src/manifest.rs`), so no
    /// manifest can yet declare an internal subtree and this always
    /// returns [`InternalGrant::none`]. Once K0 lands `requires`/`optional`
    /// parsing, this resolves the same way: collect every entry whose id is
    /// `field:vosu.osu.internal` or names a pointer below it, namespace-
    /// stripped.
    pub fn from_manifest(_manifest: &vosu_plugin_api::PluginManifest) -> Self {
        Self::none()
    }

    /// Whether every token of `subtree` is covered by a granted subtree
    /// (used by [`whole_namespace_requirement_does_not_grant_internal`]-
    /// style callers that already resolved a selector to tokens).
    fn fully_grants(&self, path: &[Box<str>]) -> bool {
        self.subtrees
            .iter()
            .any(|g| tokens_prefix(g.tokens(), path))
    }

    /// Whether some granted subtree is at or below `path` (so a deeper
    /// descendant may still be visible even though `path` itself isn't).
    fn has_granted_descendant(&self, path: &[Box<str>]) -> bool {
        self.subtrees
            .iter()
            .any(|g| tokens_prefix(path, g.tokens()))
    }
}

/// Resolves an [`Audience`] to the grant it reads `internal` through: `None`
/// for `Api` (unconditional strip), `Some` for a plugin's own grant.
fn audience_grant<'g>(audience: &Audience<'g>) -> Option<&'g InternalGrant> {
    match audience {
        Audience::Api => None,
        Audience::Plugin(grant) => Some(grant),
    }
}

enum Visibility {
    /// Not inside, nor an ancestor of, any hidden subtree: keep as-is.
    Visible,
    /// Inside a hidden subtree this audience has no grant for: omit.
    Hidden,
    /// Either an ancestor of a hidden subtree, or inside one with a
    /// granted descendant somewhere below: structurally recurse.
    Recurse,
}

fn visibility(path: &[Box<str>], hidden: &[Pointer], grant: Option<&InternalGrant>) -> Visibility {
    let hidden_here = hidden.iter().any(|h| tokens_prefix(h.tokens(), path));
    if !hidden_here {
        let ancestor_of_hidden = hidden.iter().any(|h| tokens_prefix(path, h.tokens()));
        return if ancestor_of_hidden {
            Visibility::Recurse
        } else {
            Visibility::Visible
        };
    }
    match grant {
        None => Visibility::Hidden,
        Some(grant) => {
            if grant.fully_grants(path) {
                Visibility::Visible
            } else if grant.has_granted_descendant(path) {
                Visibility::Recurse
            } else {
                Visibility::Hidden
            }
        }
    }
}

/// Redacts `value` (found at `path`, already pushed onto the working
/// token buffer) for `audience`. `None` means "omit entirely" (the caller
/// drops the key, or reports the selector absent); `Some(Cow::Borrowed)`
/// means no hidden subtree touched this value at all.
fn redact<'a>(
    value: &'a StateValue,
    path: &mut Vec<Box<str>>,
    hidden: &[Pointer],
    grant: Option<&InternalGrant>,
) -> Option<Cow<'a, StateValue>> {
    match visibility(path, hidden, grant) {
        Visibility::Hidden => None,
        Visibility::Visible => Some(Cow::Borrowed(value)),
        Visibility::Recurse => {
            let StateValue::Object(obj) = value else {
                // `x-visibility` only ever marks an object subtree (D2); a
                // hidden/granted boundary can never fall inside an array or
                // scalar. Treat defensively as hidden rather than risk
                // leaking it.
                return None;
            };
            let mut changed = false;
            let mut kept = Vec::with_capacity(obj.len());
            for (key, child) in obj.iter() {
                path.push(Box::from(key));
                match redact(child, path, hidden, grant) {
                    Some(Cow::Borrowed(b)) => kept.push((Arc::from(key), b.clone())),
                    Some(Cow::Owned(o)) => {
                        changed = true;
                        kept.push((Arc::from(key), o));
                    }
                    None => changed = true,
                }
                path.pop();
            }
            if changed {
                Some(Cow::Owned(StateValue::Object(Arc::new(StateObject::new(
                    kept,
                )))))
            } else {
                Some(Cow::Borrowed(value))
            }
        }
    }
}

/// Projects one namespace's whole value for `audience` (D2). Borrows when
/// the namespace has no hidden subtree at all (every namespace but
/// `vosu.osu`, and `vosu.osu` itself under a full `internal` grant).
pub fn project_namespace<'a>(
    reg: &SchemaRegistry,
    ns: &str,
    v: &'a StateValue,
    audience: Audience<'_>,
) -> Cow<'a, StateValue> {
    let hidden = reg.hidden_subtrees(ns);
    let grant = audience_grant(&audience);
    let mut path = Vec::new();
    redact(v, &mut path, hidden, grant)
        .expect("a namespace root is never itself a hidden subtree (D2)")
}

/// Projects one pointer read below `ns` for `audience`. `None` is "absent"
/// (`value: null, error: null`): either nothing is published there, or it
/// is hidden and ungranted — the same wire shape either way (MG).
pub fn project_select<'a>(
    reg: &SchemaRegistry,
    s: &'a SessionState,
    ns: &str,
    p: &Pointer,
    audience: Audience<'_>,
) -> Option<Cow<'a, StateValue>> {
    let value = s.get(ns, p)?;
    let hidden = reg.hidden_subtrees(ns);
    let grant = audience_grant(&audience);
    let mut path: Vec<Box<str>> = p.tokens().to_vec();
    redact(value, &mut path, hidden, grant)
}

/// Projects the whole session document (every namespace) for `audience`:
/// `SessionState::api_json`'s source, and every subscription/session-event
/// fan-out's input.
pub fn project_document(
    reg: &SchemaRegistry,
    s: &SessionState,
    audience: Audience<'_>,
) -> Vec<(Arc<str>, StateValue)> {
    let grant = audience_grant(&audience);
    s.namespaces()
        .map(|(ns, v)| {
            let hidden = reg.hidden_subtrees(ns);
            let mut path = Vec::new();
            let projected = redact(v, &mut path, hidden, grant)
                .expect("a namespace root is never itself a hidden subtree (D2)");
            (Arc::from(ns), projected.into_owned())
        })
        .collect()
}

/// A session's API v1 projection, rendered as compact JSON on `Display`/
/// `Debug` (`internal` always stripped) — the shape every debug log must
/// use instead of the raw document (D2, E5).
pub struct Redacted<'a>(pub &'a SessionState, pub &'a SchemaRegistry);

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let doc = project_document(self.1, self.0, Audience::Api);
        let mut out = Vec::new();
        write_document_json(&doc, &mut out);
        f.write_str(&String::from_utf8_lossy(&out))
    }
}

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
