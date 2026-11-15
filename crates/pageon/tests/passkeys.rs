//! Passkeys: SimpleWebAuthn registers and signs in against a fixture relying party; the
//! responses are verified as a server verifies them; keys stay in the broker.

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::*;
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use pageon::{LoadOptions, Passkeys, Replay, Sandbox, SandboxOptions, Session};
use sha2::Digest;

const RP: &str = include_str!("fixtures/passkeys/rp.html");
const LIB: &str = include_str!("fixtures/passkeys/simplewebauthn.umd.min.js");

fn shop() -> pageon_env::Log {
    site(vec![
        html("https://shop.test/", RP),
        page(
            "https://shop.test/simplewebauthn.umd.min.js",
            200,
            &[("content-type", "text/javascript")],
            LIB,
        ),
    ])
}

fn store(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "pageon-passkeys-{name}-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn b64(v: &serde_json::Value) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(v.as_str().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}: {v}"))
}

/// The JSON the page printed after `label`.
fn printed(text: &str, label: &str) -> serde_json::Value {
    let marker = format!("{label} {{");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("no {label} in {text}"))
        + label.len()
        + 1;
    let line = text[start..]
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches('"');
    let line = line.replace("\\\"", "\"");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
}

/// A relying party's checks of a registration: the client data, the rp id hash, the flags;
/// returns the credential id and public key.
fn verify_registration(r: &serde_json::Value) -> (Vec<u8>, p256::ecdsa::VerifyingKey) {
    let cd: serde_json::Value = serde_json::from_slice(&b64(&r["response"]["clientDataJSON"]))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(cd["type"], "webauthn.create");
    assert_eq!(cd["challenge"], "cmVnaXN0cmF0aW9uLWNoYWxsZW5nZQ");
    assert_eq!(cd["origin"], "https://shop.test");
    let ad = b64(&r["response"]["authenticatorData"]);
    assert_eq!(&ad[..32], sha2::Sha256::digest(b"shop.test").as_slice());
    assert_eq!(ad[32] & 0x45, 0x45, "UP, UV and AT flags");
    assert_eq!(r["response"]["publicKeyAlgorithm"], -7);
    let key = p256::ecdsa::VerifyingKey::from_public_key_der(&b64(&r["response"]["publicKey"]))
        .unwrap_or_else(|e| panic!("{e}"));
    (b64(&r["rawId"]), key)
}

/// A relying party's checks of an assertion with the registered key.
fn verify_assertion(a: &serde_json::Value, id: &[u8], key: &p256::ecdsa::VerifyingKey) {
    assert_eq!(b64(&a["rawId"]), id);
    let cdj = b64(&a["response"]["clientDataJSON"]);
    let cd: serde_json::Value = serde_json::from_slice(&cdj).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(cd["type"], "webauthn.get");
    assert_eq!(cd["challenge"], "c2lnbi1pbi1jaGFsbGVuZ2U");
    assert_eq!(cd["origin"], "https://shop.test");
    let ad = b64(&a["response"]["authenticatorData"]);
    assert_eq!(&ad[..32], sha2::Sha256::digest(b"shop.test").as_slice());
    assert_eq!(ad[32] & 0x05, 0x05, "UP and UV flags");
    let mut msg = ad.clone();
    msg.extend_from_slice(&sha2::Sha256::digest(&cdj));
    let sig = p256::ecdsa::Signature::from_der(&b64(&a["response"]["signature"]))
        .unwrap_or_else(|e| panic!("{e}"));
    key.verify(&msg, &sig)
        .unwrap_or_else(|e| panic!("signature: {e}"));
    assert_eq!(b64(&a["response"]["userHandle"]), b"user-1");
}

trait Driver {
    fn click(&mut self, name: &str);
    fn out(&mut self) -> String;
}

impl<E: pageon_env::Env> Driver for pageon::Tab<'_, E> {
    fn click(&mut self, name: &str) {
        let obs = self.observe_full();
        let b = find(&obs, "button", name);
        self.act(
            format!("click {b}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }
    fn out(&mut self) -> String {
        text_of(self.document(), "out")
    }
}

impl<E: pageon_env::Env> Driver for pageon::RemoteTab<'_, E> {
    fn click(&mut self, name: &str) {
        let obs = self.observe_full().unwrap_or_else(|e| panic!("{e}"));
        let b = find(&obs, "button", name);
        self.act(
            format!("click {b}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }
    fn out(&mut self) -> String {
        self.view(
            pageon::Dump::Text,
            pageon::Viewport::default(),
            pageon::Budget::o200k(100_000),
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }
}

fn register_and_sign_in<D: Driver>(tab: &mut D) {
    tab.click("Create a passkey");
    let reg = printed(&tab.out(), "registered");
    let (id, key) = verify_registration(&reg);
    tab.click("Sign in with a passkey");
    let auth = printed(&tab.out(), "signed-in");
    verify_assertion(&auth, &id, &key);
}

#[test]
fn an_agent_registers_and_signs_in_with_a_passkey() {
    let path = store("inproc");
    let env = Passkeys::open(Replay::new(shop()), &path).unwrap_or_else(|e| panic!("{e}"));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.observe_full().contains("Shop (passkeys)"));
    register_and_sign_in(&mut tab);
    // The store holds the credential, readable only by its owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{e}"))
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A new session with the same store signs in without registering.
    let env = Passkeys::open(Replay::new(shop()), &path).unwrap_or_else(|e| panic!("{e}"));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    tab.click("Sign in with a passkey");
    assert!(tab.out().starts_with("signed-in "), "{}", tab.out());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn passkeys_work_caged_with_keys_in_the_broker() {
    let path = store("caged");
    let rec = pageon::Recording::new(Replay::new(shop()));
    let env = Passkeys::open(&rec, &path).unwrap_or_else(|e| panic!("{e}"));
    {
        let mut sb = Sandbox::new(
            &env,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                ..SandboxOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut tab = sb
            .open("https://shop.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        register_and_sign_in(&mut tab);
    }
    // No key material in the recording.
    let store_text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    let stored: serde_json::Value =
        serde_json::from_str(&store_text).unwrap_or_else(|e| panic!("{e}"));
    let pkcs8 = stored["credentials"][0]["private_key"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!pkcs8.is_empty());
    let rec_path =
        std::env::temp_dir().join(format!("pageon-passkeys-rec-{}.json", std::process::id()));
    rec.save(&rec_path).unwrap_or_else(|e| panic!("{e}"));
    let recorded = std::fs::read_to_string(&rec_path).unwrap_or_default();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&pkcs8)
        .unwrap_or_default();
    let scalar = &raw[raw.len().saturating_sub(32)..];
    assert!(!recorded.contains(&pkcs8));
    assert!(!recorded.contains(&URL_SAFE_NO_PAD.encode(scalar)));
    let _ = std::fs::remove_file(&rec_path);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn other_sites_and_silent_calls_are_refused() {
    let evil = "<button onclick=\"navigator.credentials.create({publicKey: {challenge: new Uint8Array(16),\
        rp: {id: 'shop.test', name: 'x'}, user: {id: new Uint8Array(4), name: 'a', displayName: 'a'},\
        pubKeyCredParams: [{type: 'public-key', alg: -7}]}}).then(() => out.textContent = 'made', e => out.textContent = e.name)\">Go</button><p id=out></p>";
    let silent = "<p id=out></p><script>navigator.credentials.get({publicKey: {challenge: new Uint8Array(16), rpId: 'shop.test'}})\
        .then(() => out.textContent = 'signed', e => out.textContent = e.name)</script>";
    let rsa = "<button onclick=\"navigator.credentials.create({publicKey: {challenge: new Uint8Array(16),\
        rp: {name: 'x'}, user: {id: new Uint8Array(4), name: 'a', displayName: 'a'},\
        pubKeyCredParams: [{type: 'public-key', alg: -257}]}}).then(() => out.textContent = 'made', e => out.textContent = e.name)\">Go</button><p id=out></p>";
    let path = store("refused");
    let log = site(vec![
        html("https://evil.test/", evil),
        html("https://shop.test/silent", silent),
        html("https://shop.test/rsa", rsa),
    ]);
    let env = Passkeys::open(Replay::new(log), &path).unwrap_or_else(|e| panic!("{e}"));
    let s = Session::new(&env);
    for (url, want) in [
        ("https://evil.test/", "SecurityError"),
        ("https://shop.test/rsa", "NotSupportedError"),
    ] {
        let mut tab = s
            .open(url, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        tab.click("Go");
        assert_eq!(tab.out(), want, "{url}");
    }
    let tab = s
        .open("https://shop.test/silent", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(tab.document(), "out"), "NotAllowedError");
    assert!(!path.exists(), "the store was not touched");
}

#[test]
fn insecure_pages_have_no_webauthn_and_no_store_means_no_passkeys() {
    let page_html = "<p id=out></p><script>out.textContent = [typeof PublicKeyCredential, typeof navigator.credentials].join()</script>";
    let env = Replay::new(site(vec![
        html("http://plain.test/", page_html),
        html("https://plain.test/", page_html),
    ]));
    let doc = pageon::load("http://plain.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "undefined,undefined");
    let doc = pageon::load("https://plain.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "function,object");
    let avail = "<p id=out></p><script>PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable().then(v => out.textContent = v)</script>";
    let env = Replay::new(site(vec![html("https://a.test/", avail)]));
    let doc = pageon::load("https://a.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "false");
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn a_passkey_sign_in_costs_little() {
    let page_html = "<button onclick=\"navigator.credentials.create({publicKey: {challenge: new Uint8Array(16),\
        rp: {name: 'x'}, user: {id: new Uint8Array(4), name: 'a', displayName: 'a'},\
        pubKeyCredParams: [{type: 'public-key', alg: -7}]}}).then(() => out.textContent = 'made')\">Make</button>\
        <button onclick=\"navigator.credentials.get({publicKey: {challenge: new Uint8Array(16)}}).then(() => out.textContent += '.')\">Use</button>\
        <button onclick=\"Promise.resolve().then(() => out.textContent += '.')\">Plain</button><p id=out></p>";
    let path = store("timing");
    let env = Passkeys::open(
        Replay::new(site(vec![html("https://t.test/", page_html)])),
        &path,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://t.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    tab.click("Make");
    let mut time = |name: &str| {
        (0..20)
            .map(|_| {
                #[allow(clippy::disallowed_methods)]
                let t = std::time::Instant::now();
                tab.click(name);
                t.elapsed()
            })
            .min()
            .unwrap_or_default()
    };
    let (plain, passkey) = (time("Plain"), time("Use"));
    let extra = passkey.saturating_sub(plain);
    eprintln!("plain click {plain:?}; passkey sign-in {passkey:?}; extra {extra:?}");
    assert!(extra.as_millis() < 5, "{extra:?}");
    let _ = std::fs::remove_file(&path);
}
