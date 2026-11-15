//! Signed agent identity: Web Bot Auth over HTTP Message Signatures (RFC 9421) with
//! the operator's Ed25519 key. [`crate::Live`] signs every request it sends; the key never
//! leaves the process that holds the live environment (the broker when pages are caged).
//!
//! Wire form: the legacy sf-string `Signature-Agent`, which Cloudflare's verifier accepts
//! (it rejects the dictionary form of draft-ietf-webbotauth-httpsig-protocol-01 today).

use std::path::Path;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

/// How long a request signature is valid (s): Cloudflare's "a minute is often sufficient".
pub const VALIDITY_S: u64 = 60;

/// An operator's agent identity: an Ed25519 key, its `keyid` and the Signature-Agent URL where
/// the key directory is hosted.
pub struct Identity {
    key: Ed25519KeyPair,
    seed: [u8; 32],
    keyid: String,
    agent: String,
    tag: String,
}

/// The tag request signatures carry unless told otherwise.
pub const DEFAULT_TAG: &str = "web-bot-auth";

/// Whether `tag` may be a signature's `tag`: a token of letters, digits, `-` and `_` (at most
/// 64), so it cannot change the signature's other parameters.
pub fn check_tag(tag: &str) -> Result<(), String> {
    let ok = !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "signature tag {tag:?}: use letters, digits, '-' and '_' (at most 64), e.g. agent-browser-auth"
        ))
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("keyid", &self.keyid)
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KeyFile {
    kty: String,
    crv: String,
    x: String,
    d: String,
    #[serde(default)]
    kid: Option<String>,
    agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

/// The RFC 7638 JWK thumbprint of an Ed25519 public key (RFC 8037 A.3), base64url.
pub fn thumbprint(public: &[u8]) -> String {
    let jwk = format!(
        r#"{{"crv":"Ed25519","kty":"OKP","x":"{}"}}"#,
        URL_SAFE_NO_PAD.encode(public)
    );
    URL_SAFE_NO_PAD.encode(sha256(jwk.as_bytes()))
}

fn sha256(data: &[u8]) -> Vec<u8> {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data)
        .as_ref()
        .to_vec()
}

/// An RFC 9421 signature base: one `id: value` line per component, then
/// `"@signature-params": params`. `id` is the serialized component identifier
/// (`"@authority"`, `"signature-agent";key="sig1"`).
pub fn signature_base(components: &[(&str, &str)], params: &str) -> String {
    let mut out = String::new();
    for (id, value) in components {
        out.push_str(&format!("{id}: {value}\n"));
    }
    out.push_str(&format!("\"@signature-params\": {params}"));
    out
}

/// `("a" "b");tail`: the signature parameters of these components.
pub fn signature_params(components: &[(&str, &str)], tail: &str) -> String {
    let ids: Vec<&str> = components.iter().map(|(id, _)| *id).collect();
    format!("({}){tail}", ids.join(" "))
}

impl Identity {
    /// A new identity with a fresh key, for the Signature-Agent `agent` (an `https` URL).
    pub fn generate(agent: &str) -> Result<Self, String> {
        let mut seed = [0u8; 32];
        aws_lc_rs::rand::fill(&mut seed).map_err(|_| "no randomness for the key".to_string())?;
        Self::from_seed(seed, agent)
    }

    /// The identity of a 32-byte Ed25519 seed (private key).
    pub fn from_seed(seed: [u8; 32], agent: &str) -> Result<Self, String> {
        let agent = agent.trim().to_string();
        if !agent.starts_with("https://") {
            return Err(format!(
                "the agent URL must be https:// (got {agent:?}); it is where verifiers fetch your key directory"
            ));
        }
        let key = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|e| format!("not an Ed25519 key: {e}"))?;
        let keyid = thumbprint(key.public_key().as_ref());
        Ok(Self {
            key,
            seed,
            keyid,
            agent,
            tag: DEFAULT_TAG.into(),
        })
    }

    /// Signs with `tag` instead of `web-bot-auth` (a payment network's agent program may name
    /// another, e.g. `agent-browser-auth`).
    pub fn with_tag(mut self, tag: &str) -> Result<Self, String> {
        check_tag(tag)?;
        self.tag = tag.to_string();
        Ok(self)
    }

    /// The tag request signatures carry.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Reads a key file written by [`Identity::save`] (a private JWK plus `agent`).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let hint = "create one with `pageon identity new --agent https://…`";
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e} ({hint})", path.display()))?;
        let f: KeyFile = serde_json::from_str(&text)
            .map_err(|e| format!("{}: not an identity file: {e} ({hint})", path.display()))?;
        if f.kty != "OKP" || f.crv != "Ed25519" {
            return Err(format!(
                "{}: only Ed25519 keys sign Web Bot Auth ({hint})",
                path.display()
            ));
        }
        let seed: [u8; 32] = URL_SAFE_NO_PAD
            .decode(f.d.trim_end_matches('='))
            .ok()
            .and_then(|d| d.try_into().ok())
            .ok_or_else(|| format!("{}: \"d\" is not a 32-byte key", path.display()))?;
        let mut id =
            Self::from_seed(seed, &f.agent).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(t) = &f.tag {
            id = id
                .with_tag(t)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if URL_SAFE_NO_PAD.encode(id.public_key()) != f.x.trim_end_matches('=') {
            return Err(format!(
                "{}: \"x\" does not match the private key",
                path.display()
            ));
        }
        if f.kid.as_deref().is_some_and(|k| k != id.keyid) {
            return Err(format!(
                "{}: \"kid\" is not the key's thumbprint ({})",
                path.display(),
                id.keyid
            ));
        }
        Ok(id)
    }

    /// Writes the key file, readable by its owner only.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let f = KeyFile {
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: URL_SAFE_NO_PAD.encode(self.public_key()),
            d: URL_SAFE_NO_PAD.encode(self.seed),
            kid: Some(self.keyid.clone()),
            agent: self.agent.clone(),
            tag: (self.tag != DEFAULT_TAG).then(|| self.tag.clone()),
        };
        let json = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut file = o.open(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!(
                "{} exists; pageon does not overwrite keys (choose another --out)",
                path.display()
            ),
            _ => format!("{}: {e}", path.display()),
        })?;
        std::io::Write::write_all(&mut file, format!("{json}\n").as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Whether others than the owner may read the key file (Unix).
    pub fn exposed(path: impl AsRef<Path>) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    /// The `keyid`: the key's JWK thumbprint.
    pub fn keyid(&self) -> &str {
        &self.keyid
    }

    /// The Signature-Agent URL.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// The raw public key (32 bytes).
    pub fn public_key(&self) -> &[u8] {
        self.key.public_key().as_ref()
    }

    /// Signs `base`, as base64.
    pub fn sign_base(&self, base: &str) -> String {
        STANDARD.encode(self.key.sign(base.as_bytes()).as_ref())
    }

    /// The Web Bot Auth headers of a request to `authority` (`host[:port]`), created at
    /// `created` (s since the epoch) with `nonce`: `signature-agent`, `signature-input`,
    /// `signature`.
    pub fn sign(&self, authority: &str, created: u64, nonce: &[u8]) -> [(String, String); 3] {
        let agent = format!("\"{}\"", self.agent);
        let components = [
            ("\"@authority\"", authority),
            ("\"signature-agent\"", &agent),
        ];
        let tail = format!(
            ";created={created};keyid=\"{}\";alg=\"ed25519\";expires={};nonce=\"{}\";tag=\"{}\"",
            self.keyid,
            created + VALIDITY_S,
            STANDARD.encode(nonce),
            self.tag
        );
        let params = signature_params(&components, &tail);
        let sig = self.sign_base(&signature_base(&components, &params));
        [
            ("signature-agent".into(), agent),
            ("signature-input".into(), format!("sig1={params}")),
            ("signature".into(), format!("sig1=:{sig}:")),
        ]
    }

    /// The key directory to host at `<agent>/.well-known/http-message-signatures-directory`:
    /// its body and the response headers, signed for `authority` (the directory's host) from
    /// `created` to `expires`.
    pub fn directory(
        &self,
        authority: &str,
        created: u64,
        expires: u64,
    ) -> (String, Vec<(String, String)>) {
        let body = format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"{}","x":"{}","use":"sig"}}]}}"#,
            self.keyid,
            URL_SAFE_NO_PAD.encode(self.public_key())
        );
        let digest = format!("sha-256=:{}:", STANDARD.encode(sha256(body.as_bytes())));
        let components = [
            ("\"@authority\";req", authority),
            ("\"content-digest\"", &digest),
        ];
        let tail = format!(
            ";created={created};expires={expires};keyid=\"{}\";tag=\"http-message-signatures-directory\"",
            self.keyid
        );
        let params = signature_params(&components, &tail);
        let sig = self.sign_base(&signature_base(&components, &params));
        let headers = vec![
            (
                "Content-Type".into(),
                "application/http-message-signatures-directory+json".into(),
            ),
            ("Content-Digest".into(), digest.clone()),
            ("Signature-Input".into(), format!("binding={params}")),
            ("Signature".into(), format!("binding=:{sig}:")),
        ];
        (body, headers)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn tags_are_tokens_and_signed() {
        let id = Identity::from_seed([7; 32], "https://bot.test")
            .unwrap()
            .with_tag("agent-browser-auth")
            .unwrap();
        let [_, input, _] = id.sign("shop.test", 1_700_000_000, &[1; 32]);
        assert!(
            input.1.ends_with(";tag=\"agent-browser-auth\""),
            "{}",
            input.1
        );
        assert!(check_tag("a\";keyid=\"x").is_err());
        assert!(check_tag("").is_err() && check_tag(&"a".repeat(65)).is_err());
    }

    use super::*;

    /// RFC 9421 B.1.4 `test-key-ed25519`.
    fn test_key() -> Identity {
        let d = URL_SAFE_NO_PAD
            .decode("n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU")
            .unwrap();
        Identity::from_seed(d.try_into().unwrap(), "https://signature-agent.test").unwrap()
    }

    #[test]
    fn keyid_is_the_thumbprint() {
        assert_eq!(
            test_key().keyid(),
            "poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U"
        );
        assert_eq!(
            URL_SAFE_NO_PAD.encode(test_key().public_key()),
            "JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs"
        );
    }

    #[test]
    fn rfc9421_b26() {
        let c = [
            ("\"date\"", "Tue, 20 Apr 2021 02:07:55 GMT"),
            ("\"@method\"", "POST"),
            ("\"@path\"", "/foo"),
            ("\"@authority\"", "example.com"),
            ("\"content-type\"", "application/json"),
            ("\"content-length\"", "18"),
        ];
        let params = signature_params(&c, ";created=1618884473;keyid=\"test-key-ed25519\"");
        assert_eq!(
            params,
            r#"("date" "@method" "@path" "@authority" "content-type" "content-length");created=1618884473;keyid="test-key-ed25519""#
        );
        assert_eq!(
            test_key().sign_base(&signature_base(&c, &params)),
            "wqcAqbmYJ2ji2glfAMaRy4gruYYnx2nEFN2HN6jrnDnQCK1u02Gb04v9EDgwUPiu4A0w6vuQv5lIp5WPpBKRCw=="
        );
    }

    #[test]
    fn web_bot_auth_e21_dictionary_form() {
        let c = [
            ("\"@authority\"", "example.com"),
            (
                "\"signature-agent\";key=\"sig2\"",
                "\"https://signature-agent.test\"",
            ),
        ];
        let params = signature_params(
            &c,
            ";created=1735689600;keyid=\"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U\";alg=\"ed25519\";expires=4889289600;nonce=\"n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==\";tag=\"web-bot-auth\"",
        );
        assert_eq!(
            test_key().sign_base(&signature_base(&c, &params)),
            "CGokAoY/FrPpyuzTe1Fb2kLTAH9Idw6qSFd1+D77r1firsMRCxeuLu5e6O+ek3zQgDMAzjiXtLVfqE00xnABCQ=="
        );
    }

    #[test]
    fn web_bot_auth_e22_legacy_form_is_what_requests_carry() {
        // E.2.2 with `expires` = created + 3600; `sign` uses created + 60, so rebuild it.
        let nonce = STANDARD
            .decode("e8N7S2MFd/qrd6T2R3tdfAuuANngKI7LFtKYI/vowzk4lAZYadIX6wW25MwG7DCT9RUKAJ0qVkU0mEeLElW1qg==")
            .unwrap();
        let c = [
            ("\"@authority\"", "example.com"),
            ("\"signature-agent\"", "\"https://signature-agent.test\""),
        ];
        let tail = format!(
            ";created=1735689600;keyid=\"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U\";alg=\"ed25519\";expires=1735693200;nonce=\"{}\";tag=\"web-bot-auth\"",
            STANDARD.encode(&nonce)
        );
        let params = signature_params(&c, &tail);
        assert_eq!(
            test_key().sign_base(&signature_base(&c, &params)),
            "jdq0SqOwHdyHr9+r5jw3iYZH6aNGKijYp/EstF4RQTQdi5N5YYKrD+mCT1HA1nZDsi6nJKuHxUi/5Syp3rLWBA=="
        );
        // `sign` emits the same structure (only `expires` differs).
        let h = test_key().sign("example.com", 1_735_689_600, &nonce);
        assert_eq!(h[0].1, "\"https://signature-agent.test\"");
        assert_eq!(
            h[1].1,
            format!(
                "sig1={}",
                params.replace("expires=1735693200", "expires=1735689660")
            )
        );
        assert!(h[2].1.starts_with("sig1=:") && h[2].1.ends_with(':'));
    }

    #[test]
    fn web_bot_auth_e23_signed_directory() {
        let (body, headers) =
            test_key().directory("signature-agent.test", 1_735_689_600, 4_889_289_600);
        assert_eq!(
            body,
            r#"{"keys":[{"kty":"OKP","crv":"Ed25519","kid":"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U","x":"JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs","use":"sig"}]}"#
        );
        assert_eq!(
            headers[1].1,
            "sha-256=:CADMT2aBdV/rqQr/NIru64ERQkCobVvllA4V0fLFDu0=:"
        );
        assert_eq!(
            headers[3].1,
            "binding=:l6P8R67tm3kujAxbHWio7ll01qrEZ0dKD/WWlGhNYEmTnFZM8Wt0VQ9zqGfvo7T/UMkBxsigzChM1Gpz7gOVBg==:"
        );
    }

    #[test]
    fn key_files_round_trip_and_stay_private() {
        let dir = std::env::temp_dir().join(format!("pageon-id-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let id = Identity::generate("https://bot.example").unwrap();
        id.save(&dir).unwrap();
        assert!(!Identity::exposed(&dir));
        let back = Identity::load(&dir).unwrap();
        assert_eq!(back.keyid(), id.keyid());
        assert_eq!(back.agent(), "https://bot.example");
        assert!(id.save(&dir).unwrap_err().contains("does not overwrite"));
        assert!(!format!("{back:?}").contains(&URL_SAFE_NO_PAD.encode(back.seed)));
        std::fs::remove_file(&dir).unwrap();
        assert!(
            Identity::generate("http://bot.example")
                .unwrap_err()
                .contains("https")
        );
    }
}
