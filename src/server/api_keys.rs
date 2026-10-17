//! Persistent, explicitly scoped user credentials. Secrets never leave creation responses.
use axum::http::{HeaderMap, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Scope {
    #[serde(rename = "settings:write")]
    SettingsWrite,
    #[serde(rename = "profiles:manage")]
    ProfilesManage,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    #[serde(flatten)]
    entry: Entry,
    sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Entry {
    id: String,
    name: String,
    scopes: Vec<Scope>,
    display: String,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_used: Option<String>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    keys: Vec<StoredEntry>,
}
struct State {
    document: Document,
    used: std::collections::HashMap<String, Instant>,
    denied: Option<Instant>,
    revocations: std::collections::HashMap<String, tokio_util::sync::CancellationToken>,
}
pub struct ApiKeys {
    path: PathBuf,
    state: Mutex<State>,
    logger: Option<crate::logging::Logger>,
}
#[derive(Clone)]
pub(crate) struct KeyGrant {
    pub store: std::sync::Arc<ApiKeys>,
    pub id: String,
    scope: Scope,
    pub revoked: tokio_util::sync::CancellationToken,
}
impl KeyGrant {
    pub fn allowed(&self) -> bool {
        !self.revoked.is_cancelled() && self.store.touch_id(&self.id, self.scope)
    }
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
fn random_hex() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OS randomness unavailable")?;
    Ok(hex(&bytes))
}
fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.chars().count())
        && name.chars().all(|c| !c.is_control()
            && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
impl ApiKeys {
    pub fn load(path: PathBuf, logger: Option<crate::logging::Logger>) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(metadata) = fs::metadata(&path) {
                // SAFETY: geteuid reads process credentials without pointer arguments.
                let owner = unsafe { libc::geteuid() };
                if (metadata.uid() != owner || metadata.mode() & 0o077 != 0)
                    && let Some(log) = &logger
                {
                    log.warn("api-keys.toml ownership or permissions are not private to the current user; keys remain enabled");
                }
            }
        }
        let loaded = (|| -> Result<Document, String> {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Document::default()),
                Err(_) => return Err("unreadable file".into()),
            };
            let doc: Document = toml::from_str(&text).map_err(|_| "malformed file")?;
            if doc.keys.len() > 64 {
                return Err("too many keys".into());
            }
            let mut ids = std::collections::HashSet::new();
            for key in &doc.keys {
                let entry = &key.entry;
                if !is_hex(&key.sha256, 64)
                    || !is_hex(&entry.id, 64)
                    || !ids.insert(&entry.id)
                    || !valid_name(&entry.name)
                    || entry.scopes.is_empty()
                    || entry.scopes.len() > 2
                    || (entry.scopes.len() == 2 && entry.scopes[0] == entry.scopes[1])
                    || !entry
                        .display
                        .strip_prefix("vosu_k_")
                        .is_some_and(|v| is_hex(v, 4))
                    || chrono::DateTime::parse_from_rfc3339(&entry.created).is_err()
                    || entry
                        .last_used
                        .as_ref()
                        .is_some_and(|v| chrono::DateTime::parse_from_rfc3339(v).is_err())
                {
                    return Err("invalid entries".into());
                }
            }
            Ok(doc)
        })();
        let document = loaded.unwrap_or_else(|_| {
            if let Some(log) = &logger { log.warn("API keys disabled: invalid or unreadable api-keys.toml; manage keys to replace it"); }
            Document::default()
        });
        Self {
            path,
            state: Mutex::new(State {
                document,
                used: Default::default(),
                denied: None,
                revocations: Default::default(),
            }),
            logger,
        }
    }
    fn save(&self, document: &Document) -> Result<(), String> {
        let parent = self.path.parent().ok_or("missing key directory")?;
        fs::create_dir_all(parent).map_err(|_| "cannot create key directory")?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "cannot create key file")?;
        let text = toml::to_string(document).map_err(|_| "cannot serialize keys")?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.as_file().sync_all())
            .map_err(|_| "cannot save keys")?;
        // ReplaceFileW fails with a sharing violation while the replacement is still open.
        let file = file.into_temp_path();
        crate::platform::atomic_replace(&file, &self.path).map_err(|_| "cannot save keys".into())
    }
    pub fn list(&self) -> Vec<Entry> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .document
            .keys
            .iter()
            .map(|k| k.entry.clone())
            .collect()
    }
    pub fn manage(&self, value: &serde_json::Value) -> Result<serde_json::Value, String> {
        let field = |name| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("missing {name}"))
        };
        let action = field("action")?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.document.clone();
        let response;
        let logged;
        match action {
            "create" => {
                if next.keys.len() >= 64 {
                    return Err("maximum 64 API keys".into());
                }
                let name = field("name")?;
                if !valid_name(name) {
                    return Err("name must contain 1–64 printable characters".into());
                }
                let scopes: Vec<Scope> =
                    serde_json::from_value(value.get("scopes").cloned().ok_or("missing scopes")?)
                        .map_err(|_| "invalid scopes")?;
                if scopes.is_empty()
                    || scopes.len() > 2
                    || (scopes.len() == 2 && scopes[0] == scopes[1])
                {
                    return Err("select one or both scopes".into());
                }
                let key = format!("vosu_k_{}", random_hex()?);
                let entry = Entry {
                    id: random_hex()?,
                    name: name.into(),
                    scopes,
                    display: key[..11].into(),
                    created: now(),
                    last_used: None,
                };
                logged = format!("API key created: {} ({})", entry.name, entry.id);
                response = serde_json::json!({"key": key, "entry": entry});
                next.keys.push(StoredEntry {
                    entry,
                    sha256: hex(&Sha256::digest(key.as_bytes())),
                });
            }
            "rename" | "revoke" => {
                let id = field("id")?;
                let index = next
                    .keys
                    .iter()
                    .position(|key| key.entry.id == id)
                    .ok_or("unknown key")?;
                logged = format!("API key {action}: {} ({id})", next.keys[index].entry.name);
                if action == "rename" {
                    let name = field("name")?;
                    if !valid_name(name) {
                        return Err("name must contain 1–64 printable characters".into());
                    }
                    next.keys[index].entry.name = name.into();
                } else {
                    next.keys.remove(index);
                }
                response = serde_json::json!({"status":"success"});
            }
            _ => return Err("unknown action".into()),
        }
        self.save(&next)?;
        state
            .used
            .retain(|id, _| next.keys.iter().any(|key| key.entry.id == *id));
        state.revocations.retain(|id, token| {
            let retained = next.keys.iter().any(|key| key.entry.id == *id);
            if !retained {
                token.cancel();
            }
            retained
        });
        state.document = next;
        if let Some(log) = &self.logger {
            log.info(logged);
        }
        Ok(response)
    }
    pub(crate) fn grant(self: &std::sync::Arc<Self>, id: String, scope: Scope) -> KeyGrant {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let revoked = if state
            .document
            .keys
            .iter()
            .any(|key| key.entry.id == id && key.entry.scopes.contains(&scope))
        {
            state.revocations.entry(id.clone()).or_default().clone()
        } else {
            // Revocation may race the interval between verification and admission.
            let token = tokio_util::sync::CancellationToken::new();
            token.cancel();
            token
        };
        KeyGrant {
            store: self.clone(),
            id,
            scope,
            revoked,
        }
    }
    pub(crate) fn verify(&self, key: &str, scope: Scope) -> Result<String, ()> {
        let digest = hex(&Sha256::digest(key.as_bytes()));
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut found = None;
        for stored in &state.document.keys {
            let equal = bool::from(stored.sha256.as_bytes().ct_eq(digest.as_bytes()));
            if equal && stored.entry.scopes.contains(&scope) {
                found = Some(stored.entry.id.clone());
            }
        }
        if !key.strip_prefix("vosu_k_").is_some_and(|v| is_hex(v, 64)) {
            found = None;
        }
        let Some(id) = found else {
            if state
                .denied
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
            {
                if let Some(log) = &self.logger {
                    log.warn("API key authentication denied");
                }
                state.denied = Some(Instant::now());
            }
            return Err(());
        };
        self.touch(&mut state, &id);
        Ok(id)
    }
    fn touch(&self, state: &mut State, id: &str) {
        if state
            .used
            .get(id)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        // Preserve the write bound across daemon restarts as well.
        if state
            .document
            .keys
            .iter()
            .find(|key| key.entry.id == id)
            .and_then(|key| key.entry.last_used.as_deref())
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .is_some_and(|time| chrono::Utc::now().signed_duration_since(time).num_seconds() < 60)
        {
            return;
        }
        state.used.insert(id.into(), Instant::now());
        if let Some(key) = state.document.keys.iter_mut().find(|k| k.entry.id == id) {
            key.entry.last_used = Some(now());
        }
        if self.save(&state.document).is_err()
            && let Some(log) = &self.logger
        {
            log.warn("API key last_used could not be saved");
        }
    }
    fn touch_id(&self, id: &str, scope: Scope) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let valid = state
            .document
            .keys
            .iter()
            .any(|k| k.entry.id == id && k.entry.scopes.contains(&scope));
        if valid {
            self.touch(&mut state, id);
        }
        valid
    }
}
/// Only HTTP headers carry credentials. Query strings are deliberately ignored.
pub(crate) fn presented(
    headers: &HeaderMap,
    websocket: bool,
) -> Result<Option<(String, bool)>, ()> {
    let mut key = None;
    for value in headers.get_all(header::AUTHORIZATION) {
        let text = value.to_str().map_err(|_| ())?;
        if let Some((scheme, value)) = text.split_once(' ')
            && scheme.eq_ignore_ascii_case("bearer")
            && value.starts_with("vosu_k_")
        {
            if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
                return Err(());
            }
            key = Some((value.to_owned(), false));
        }
    }
    if websocket {
        let mut marker = false;
        let mut secret = None;
        for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
            for protocol in value.to_str().map_err(|_| ())?.split(',').map(str::trim) {
                if protocol == "vosu.key" {
                    marker = true;
                }
                if let Some(value) = protocol.strip_prefix("vosu.key.")
                    && secret.replace(value.to_owned()).is_some()
                {
                    return Err(());
                }
            }
        }
        if let Some(secret) = secret {
            if !marker || key.is_some() {
                return Err(());
            }
            key = Some((secret, true));
        } else if marker {
            return Err(());
        }
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn creation_hash_only_atomic_permissions_scopes_revocation_and_throttle() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("api-keys.toml");
        let store = std::sync::Arc::new(ApiKeys::load(path.clone(), None));
        let created = store
            .manage(&json!({"action":"create","name":"Test","scopes":["settings:write"]}))
            .unwrap();
        let secret = created["key"].as_str().unwrap();
        let id = created["entry"]["id"].as_str().unwrap();
        assert!(
            secret
                .strip_prefix("vosu_k_")
                .is_some_and(|s| is_hex(s, 64))
        );
        let bytes = fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains(secret));
        assert!(bytes.contains(&hex(&Sha256::digest(secret.as_bytes()))));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            store.verify(secret, Scope::SettingsWrite),
            Ok(id.to_owned())
        );
        assert!(store.verify(secret, Scope::ProfilesManage).is_err());
        let wrong = format!("vosu_k_{}", "0".repeat(64));
        assert!(store.verify(&wrong, Scope::SettingsWrite).is_err());
        let used = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(store.verify(secret, Scope::SettingsWrite).is_ok());
        assert_eq!(modified, fs::metadata(&path).unwrap().modified().unwrap());
        assert_eq!(used, fs::read(&path).unwrap());
        let restored = ApiKeys::load(path.clone(), None);
        assert!(restored.verify(secret, Scope::SettingsWrite).is_ok());
        assert!(
            !serde_json::to_string(&store.list())
                .unwrap()
                .contains("sha256")
        );
        let grant = store.grant(id.into(), Scope::SettingsWrite);
        assert!(grant.allowed());
        store
            .manage(&json!({"action":"rename","id":id,"name":"Renamed"}))
            .unwrap();
        assert!(grant.allowed());
        store.manage(&json!({"action":"revoke","id":id})).unwrap();
        assert!(!grant.allowed());
        assert!(grant.revoked.is_cancelled());
        assert!(
            store
                .grant(id.into(), Scope::SettingsWrite)
                .revoked
                .is_cancelled()
        );
        assert!(store.verify(secret, Scope::SettingsWrite).is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn names_reject_bidi_controls() {
        assert!(valid_name("Streaming – 日本語"));
        for control in [
            '\n', '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}',
            '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert!(!valid_name(&format!("key{control}name")));
        }
    }

    #[test]
    fn malformed_file_disabled_and_untouched_until_management() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("api-keys.toml");
        fs::write(&path, "not valid TOML").unwrap();
        let store = ApiKeys::load(path.clone(), None);
        assert!(store.list().is_empty());
        assert!(
            store
                .verify(&format!("vosu_k_{}", "0".repeat(64)), Scope::SettingsWrite)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "not valid TOML");
        assert!(
            store
                .manage(&json!({"action":"create","name":"Recovery","scopes":["profiles:manage"]}))
                .is_ok()
        );
        assert_eq!(ApiKeys::load(path, None).list().len(), 1);
    }

    #[test]
    fn failed_atomic_replacement_does_not_publish_mutation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("api-keys.toml");
        let store = ApiKeys::load(path.clone(), None);
        store
            .manage(&json!({"action":"create","name":"Original","scopes":["settings:write"]}))
            .unwrap();
        let before = fs::read(&path).unwrap();
        fs::rename(&path, root.path().join("saved.toml")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            store
                .manage(&json!({"action":"create","name":"Failed","scopes":["profiles:manage"]}))
                .is_err()
        );
        assert_eq!(store.list().len(), 1);
        assert_eq!(fs::read(root.path().join("saved.toml")).unwrap(), before);
    }

    #[test]
    fn bearer_and_browser_protocol_are_unambiguous() {
        let secret = format!("vosu_k_{}", "a".repeat(64));
        let mut headers = HeaderMap::new();
        assert_eq!(presented(&headers, true), Ok(None));
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            format!("vosu.key, vosu.key.{secret}").parse().unwrap(),
        );
        assert_eq!(presented(&headers, true), Ok(Some((secret.clone(), true))));
        assert_eq!(presented(&headers, false), Ok(None));
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        assert!(presented(&headers, true).is_err());
        assert_eq!(presented(&headers, false), Ok(Some((secret, false))));
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", "a".repeat(64)).parse().unwrap(),
        );
        assert_eq!(presented(&headers, false), Ok(None));
    }
}
