//! Passkeys: the operator's WebAuthn credentials, held and used in the broker.
//!
//! A page asks through [`Env::passkey`]; the answer carries credential ids, public keys and
//! signatures. Private keys stay in this process and in the store file (0600). The rp id is
//! checked against the page's origin here, and the authenticator data signed is built here,
//! so a page cannot have a credential used for another site.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p256::ecdsa::signature::Signer;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use pageon_env::{Env, NetError, PasskeyCall, PasskeyReply, Request, Response};
use sha2::Digest;

/// A passkey store could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyError(pub String);

impl std::fmt::Display for PasskeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PasskeyError {}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Stored {
    /// Credential id, base64url.
    id: String,
    rp_id: String,
    /// User handle, base64url.
    user_id: String,
    user_name: String,
    /// PKCS#8 DER, base64.
    private_key: String,
    /// When it was created (ms since the epoch).
    created_ms: u64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Store {
    version: u32,
    credentials: Vec<Stored>,
}

/// Whether `path` holds a passkey store (or does not exist yet).
pub(crate) fn check_store(path: &Path) -> Result<(), PasskeyError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Store>(&text)
            .map(|_| ())
            .map_err(|e| PasskeyError(format!("{}: not a passkey store: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PasskeyError(format!("{}: {e}", path.display()))),
    }
}

/// An environment with a passkey store around `inner` (see the module docs).
pub struct Passkeys<E> {
    inner: E,
    store: RefCell<Store>,
    path: Option<PathBuf>,
}

impl<E> std::fmt::Debug for Passkeys<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passkeys")
            .field("path", &self.path)
            .field("credentials", &self.store.borrow().credentials.len())
            .finish_non_exhaustive()
    }
}

impl<E: Env> Passkeys<E> {
    /// A store kept in `path` (created with mode 0600 on the first new credential).
    pub fn open(inner: E, path: impl Into<PathBuf>) -> Result<Self, PasskeyError> {
        let path = path.into();
        let store = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Store>(&text).map_err(|e| {
                PasskeyError(format!("{}: not a passkey store: {e}", path.display()))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store {
                version: 1,
                credentials: Vec::new(),
            },
            Err(e) => return Err(PasskeyError(format!("{}: {e}", path.display()))),
        };
        Ok(Self {
            inner,
            store: RefCell::new(store),
            path: Some(path),
        })
    }

    /// A store that lives as long as the session.
    pub fn in_memory(inner: E) -> Self {
        Self {
            inner,
            store: RefCell::new(Store {
                version: 1,
                credentials: Vec::new(),
            }),
            path: None,
        }
    }

    /// The environment inside.
    pub fn inner(&self) -> &E {
        &self.inner
    }

    /// Whether a store file is readable by other users.
    pub fn exposed(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    fn save(&self) -> Result<(), NetError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(&*self.store.borrow())
            .map_err(|e| NetError::Io(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = opts.open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| NetError::Io(format!("{}: {e}", path.display())))
    }

    fn create(
        &self,
        origin: &str,
        rp_id: &str,
        user_id: &[u8],
        user_name: &str,
        exclude: &[Vec<u8>],
        client_data_hash: &[u8],
    ) -> Result<PasskeyReply, NetError> {
        if let Some(refused) = rp_refusal(origin, rp_id) {
            return Ok(refused);
        }
        let excluded =
            self.store.borrow().credentials.iter().any(|c| {
                c.rp_id == rp_id && exclude.iter().any(|x| URL_SAFE_NO_PAD.encode(x) == c.id)
            });
        if excluded {
            return Ok(refused(
                "InvalidStateError",
                "the authenticator already holds one of the excluded credentials",
            ));
        }
        let key = new_key()?;
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| NetError::Io(e.to_string()))?;
        let public = key.public_key();
        let sec1 = p256::ecdsa::VerifyingKey::from(&public)
            .to_sec1_point(false)
            .as_bytes()
            .to_vec();
        // 0x04 ‖ x ‖ y.
        let cose = cose_es256(&sec1[1..33], &sec1[33..65]);
        let mut auth = authenticator_data(rp_id, FLAG_UP | FLAG_UV | FLAG_AT);
        auth.extend_from_slice(&[0u8; 16]);
        auth.extend_from_slice(&(id.len() as u16).to_be_bytes());
        auth.extend_from_slice(&id);
        auth.extend_from_slice(&cose);
        let attestation = attestation_none(&auth);
        let spki = public
            .to_public_key_der()
            .map_err(|e| NetError::Io(e.to_string()))?
            .as_bytes()
            .to_vec();
        let pkcs8 = key
            .to_pkcs8_der()
            .map_err(|e| NetError::Io(e.to_string()))?
            .as_bytes()
            .to_vec();
        let _ = client_data_hash;
        let created_ms = self.inner.now_ms().unwrap_or(0);
        {
            let mut store = self.store.borrow_mut();
            let user = URL_SAFE_NO_PAD.encode(user_id);
            // A new credential for the same account replaces the old one.
            store
                .credentials
                .retain(|c| !(c.rp_id == rp_id && c.user_id == user));
            store.credentials.push(Stored {
                id: URL_SAFE_NO_PAD.encode(id),
                rp_id: rp_id.to_string(),
                user_id: user,
                user_name: user_name.to_string(),
                private_key: STANDARD.encode(pkcs8),
                created_ms,
            });
        }
        self.save()?;
        Ok(PasskeyReply::Created {
            id: id.to_vec(),
            authenticator_data: auth,
            attestation_object: attestation,
            public_key: spki,
        })
    }

    fn get(
        &self,
        origin: &str,
        rp_id: &str,
        allow: &[Vec<u8>],
        client_data_hash: &[u8],
    ) -> Result<PasskeyReply, NetError> {
        if let Some(refused) = rp_refusal(origin, rp_id) {
            return Ok(refused);
        }
        let allowed: Vec<String> = allow.iter().map(|a| URL_SAFE_NO_PAD.encode(a)).collect();
        // Without a picker, the newest matching credential signs.
        let chosen = self
            .store
            .borrow()
            .credentials
            .iter()
            .filter(|c| c.rp_id == rp_id && (allowed.is_empty() || allowed.contains(&c.id)))
            .max_by_key(|c| c.created_ms)
            .cloned();
        let Some(c) = chosen else {
            return Ok(refused(
                "NotAllowedError",
                &format!("no passkey for {rp_id} in the store"),
            ));
        };
        let der = STANDARD
            .decode(&c.private_key)
            .map_err(|e| NetError::Io(e.to_string()))?;
        let key = p256::SecretKey::from_pkcs8_der(&der)
            .map_err(|e| NetError::Io(format!("a stored key does not load: {e}")))?;
        let auth = authenticator_data(rp_id, FLAG_UP | FLAG_UV);
        let mut message = auth.clone();
        message.extend_from_slice(client_data_hash);
        let signer = p256::ecdsa::SigningKey::from(&key);
        let signature: p256::ecdsa::Signature = signer.sign(&message);
        Ok(PasskeyReply::Signed {
            id: URL_SAFE_NO_PAD
                .decode(&c.id)
                .map_err(|e| NetError::Io(e.to_string()))?,
            authenticator_data: auth,
            signature: signature.to_der().as_bytes().to_vec(),
            user_handle: URL_SAFE_NO_PAD
                .decode(&c.user_id)
                .map_err(|e| NetError::Io(e.to_string()))?,
        })
    }
}

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

fn refused(name: &str, message: &str) -> PasskeyReply {
    PasskeyReply::Refused {
        name: name.into(),
        message: message.into(),
    }
}

/// WebAuthn's rp id rule: the page's host, or a registrable suffix of it that is not a public
/// suffix, on https (or localhost).
fn rp_refusal(origin: &str, rp_id: &str) -> Option<PasskeyReply> {
    let url = url::Url::parse(origin).ok();
    let host = url
        .as_ref()
        .and_then(|u| u.domain())
        .map(str::to_ascii_lowercase);
    let secure = url.as_ref().is_some_and(|u| {
        u.scheme() == "https" || (u.scheme() == "http" && u.host_str() == Some("localhost"))
    });
    let rp = rp_id.to_ascii_lowercase();
    let ok = secure
        && host.as_deref().is_some_and(|h| {
            h == rp || (h.ends_with(&format!(".{rp}")) && !pageon_net::site::is_public_suffix(&rp))
        });
    (!ok).then(|| {
        refused(
            "SecurityError",
            &format!("the relying party id {rp_id} is not valid for {origin}"),
        )
    })
}

/// A P-256 key from the operating system's randomness (never the session's).
fn new_key() -> Result<p256::SecretKey, NetError> {
    loop {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| NetError::Io(e.to_string()))?;
        if let Ok(k) = p256::SecretKey::from_slice(&bytes) {
            return Ok(k);
        }
    }
}

/// The authenticator data's fixed part: rp id hash, flags, counter 0.
fn authenticator_data(rp_id: &str, flags: u8) -> Vec<u8> {
    let mut d = sha2::Sha256::digest(rp_id.as_bytes()).to_vec();
    d.push(flags);
    d.extend_from_slice(&0u32.to_be_bytes());
    d
}

// ----- CBOR, for the two fixed shapes -----

fn cbor_head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xff => out.extend_from_slice(&[m | 24, n as u8]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        _ => {
            out.push(m | 26);
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
    }
}

fn cbor_int(out: &mut Vec<u8>, v: i64) {
    if v >= 0 {
        cbor_head(out, 0, v as u64);
    } else {
        cbor_head(out, 1, (-1 - v) as u64);
    }
}

fn cbor_bytes(out: &mut Vec<u8>, b: &[u8]) {
    cbor_head(out, 2, b.len() as u64);
    out.extend_from_slice(b);
}

fn cbor_text(out: &mut Vec<u8>, s: &str) {
    cbor_head(out, 3, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// The COSE key of an ES256 public key (CTAP2 canonical order).
fn cose_es256(x: &[u8], y: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    cbor_head(&mut o, 5, 5);
    cbor_int(&mut o, 1);
    cbor_int(&mut o, 2);
    cbor_int(&mut o, 3);
    cbor_int(&mut o, -7);
    cbor_int(&mut o, -1);
    cbor_int(&mut o, 1);
    cbor_int(&mut o, -2);
    cbor_bytes(&mut o, x);
    cbor_int(&mut o, -3);
    cbor_bytes(&mut o, y);
    o
}

/// `{"fmt": "none", "attStmt": {}, "authData": …}`.
fn attestation_none(auth: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    cbor_head(&mut o, 5, 3);
    cbor_text(&mut o, "fmt");
    cbor_text(&mut o, "none");
    cbor_text(&mut o, "attStmt");
    cbor_head(&mut o, 5, 0);
    cbor_text(&mut o, "authData");
    cbor_bytes(&mut o, auth);
    o
}

impl<E: Env> Env for Passkeys<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        self.inner.fetch(req)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        self.inner.now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        self.inner.random_u64()
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        self.inner.secret(name, origin)
    }

    fn file(&self, name: &str, origin: &str) -> Result<pageon_env::StagedFile, NetError> {
        self.inner.file(name, origin)
    }

    fn passkey(&self, call: &PasskeyCall) -> Result<PasskeyReply, NetError> {
        match call {
            PasskeyCall::Available => Ok(PasskeyReply::Available(true)),
            PasskeyCall::Create {
                origin,
                rp_id,
                user_id,
                user_name,
                exclude,
                client_data_hash,
            } => self.create(origin, rp_id, user_id, user_name, exclude, client_data_hash),
            PasskeyCall::Get {
                origin,
                rp_id,
                allow,
                client_data_hash,
            } => self.get(origin, rp_id, allow, client_data_hash),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rp_ids_follow_the_origin() {
        assert!(rp_refusal("https://login.shop.test", "shop.test").is_none());
        assert!(rp_refusal("https://shop.test", "shop.test").is_none());
        assert!(rp_refusal("https://evil.test", "shop.test").is_some());
        assert!(rp_refusal("https://shop.co.uk", "co.uk").is_some());
        assert!(rp_refusal("http://shop.test", "shop.test").is_some());
        assert!(rp_refusal("http://localhost:8080", "localhost").is_none());
    }

    #[test]
    fn cbor_shapes() {
        let k = cose_es256(&[1; 32], &[2; 32]);
        assert_eq!(&k[..7], &[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01]);
        let a = attestation_none(&[9, 9]);
        assert_eq!(a[0], 0xa3);
        assert!(a.ends_with(&[0x42, 9, 9]));
    }
}
