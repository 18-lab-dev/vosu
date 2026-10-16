//! Local native-broker credentials; never used as HTTP management authority.

use axum::http::{HeaderMap, header};
use std::{
    fs,
    io::{self, Write},
    net::IpAddr,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use subtle::ConstantTimeEq;

/// The published file is never deleted. A stopped daemon cannot authenticate its
/// stale token, and the next start atomically replaces the file; deleting it on
/// shutdown could only race (and remove) a newer daemon's credential.
pub(crate) struct BrokerToken {
    path: PathBuf,
    token: [u8; 64],
    enabled: AtomicBool,
}

impl BrokerToken {
    pub(crate) fn new(path: PathBuf) -> io::Result<Self> {
        let mut random = [0; 32];
        getrandom::fill(&mut random)
            .map_err(|_| io::Error::other("broker OS randomness unavailable"))?;
        let mut token = [0; 64];
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, byte) in random.into_iter().enumerate() {
            token[2 * i] = HEX[(byte >> 4) as usize];
            token[2 * i + 1] = HEX[(byte & 15) as usize];
        }
        Ok(Self {
            path,
            token,
            enabled: AtomicBool::new(false),
        })
    }

    pub(crate) fn publish(&self, address: std::net::SocketAddr) -> io::Result<()> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("broker path has no parent"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(directory)?;
        // tempfile creates Unix files with mode 0600 at creation, before writing.
        // On Windows the file inherits the destination directory's ACL.
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({
                "version": 1,
                "origin": format!("http://{address}"),
                "token": std::str::from_utf8(&self.token).expect("hex is ASCII"),
            }),
        )?;
        file.flush()?;
        crate::platform::atomic_replace(file.path(), &self.path)?;
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// A present, invalid credential fails closed, including on non-loopback peers.
    pub(crate) fn authorize(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> Result<bool, ()> {
        let mut bearer = None;
        for value in headers.get_all(header::AUTHORIZATION) {
            let bytes = value.as_bytes();
            let scheme_end = bytes
                .iter()
                .position(|byte| byte.is_ascii_whitespace())
                .unwrap_or(bytes.len());
            if !bytes[..scheme_end].eq_ignore_ascii_case(b"Bearer") {
                continue;
            }
            if bearer.is_some() {
                return Err(());
            }
            bearer = Some(bytes.get(scheme_end + 1..).ok_or(())?);
        }
        let Some(supplied) = bearer else {
            return Ok(false);
        };
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1
            || supplied.len() != self.token.len()
        {
            return Err(());
        }
        let valid = bool::from(self.token.ct_eq(supplied)) & self.enabled.load(Ordering::Acquire);
        if valid && peer.is_some_and(|ip| ip.to_canonical().is_loopback()) {
            Ok(true)
        } else {
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_format_rotation_and_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vosu/broker.json");
        let first = BrokerToken::new(path.clone()).unwrap();
        first.publish("127.0.0.1:32123".parse().unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["origin"], "http://127.0.0.1:32123");
        let token = value["token"].as_str().unwrap();
        assert_eq!(token.len(), 64);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let second = BrokerToken::new(path.clone()).unwrap();
        second.publish("[::1]:32124".parse().unwrap()).unwrap();
        assert_ne!(first.token, second.token);
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value["token"].as_str().unwrap().as_bytes(),
            second.token.as_slice()
        );
    }
    #[test]
    fn credentials_require_loopback_and_exact_bearer() {
        let root = tempfile::tempdir().unwrap();
        let token = BrokerToken::new(root.path().join("broker.json")).unwrap();
        token.publish("127.0.0.1:32123".parse().unwrap()).unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(
            token.authorize(&headers, Some("127.0.0.1".parse().unwrap())),
            Ok(false)
        );
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", std::str::from_utf8(&token.token).unwrap())
                .parse()
                .unwrap(),
        );
        assert_eq!(
            token.authorize(&headers, Some("127.0.0.1".parse().unwrap())),
            Ok(true)
        );
        assert_eq!(
            token.authorize(&headers, Some("::1".parse().unwrap())),
            Ok(true)
        );
        assert_eq!(
            token.authorize(&headers, Some("::ffff:127.0.0.1".parse().unwrap())),
            Ok(true)
        );
        assert_eq!(
            token.authorize(&headers, Some("192.0.2.1".parse().unwrap())),
            Err(())
        );
        assert_eq!(token.authorize(&headers, None), Err(()));
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert_eq!(
            token.authorize(&headers, Some("127.0.0.1".parse().unwrap())),
            Err(())
        );
        let loopback = Some("127.0.0.1".parse().unwrap());
        let mut wrong = token.token;
        wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", std::str::from_utf8(&wrong).unwrap())
                .parse()
                .unwrap(),
        );
        assert_eq!(token.authorize(&headers, loopback), Err(()));
        headers.insert(
            header::AUTHORIZATION,
            format!("bearer {}", std::str::from_utf8(&token.token).unwrap())
                .parse()
                .unwrap(),
        );
        assert_eq!(token.authorize(&headers, loopback), Ok(true));
        headers.append(header::AUTHORIZATION, "Basic cached".parse().unwrap());
        assert_eq!(token.authorize(&headers, loopback), Err(()));
        headers.insert(header::AUTHORIZATION, "Basic cached".parse().unwrap());
        assert_eq!(token.authorize(&headers, loopback), Ok(false));
    }
}
