//! Web Crypto's `crypto.subtle` for what sites use: SHA digests; HMAC, ECDSA (P-256) and
//! Ed25519 keys, signatures and verification; AES-GCM encryption. Keys and random nonces come
//! from the document's crypto stream (keyed through the environment, so a replay generates the
//! same keys) and ECDSA signs deterministically (RFC 6979). Other algorithms reject with
//! `NotSupportedError`.

use aws_lc_rs::{aead, digest, hmac, signature};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde_json::{Value as Json, json};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

/// A Web Crypto key.
#[derive(Debug, Clone)]
pub struct Key {
    /// `public`, `private` or `secret`.
    pub kind: &'static str,
    /// `algorithm` (as JSON: `{name, hash, length, namedCurve}`).
    pub algorithm: Json,
    /// `usages`.
    pub usages: Vec<String>,
    /// `extractable`.
    pub extractable: bool,
    /// The key itself.
    pub material: Material,
}

/// Key material.
#[derive(Debug, Clone)]
pub enum Material {
    /// HMAC and AES keys.
    Secret(Vec<u8>),
    /// An ECDSA P-256 private scalar (32 bytes), with its public key.
    EcPrivate(Vec<u8>, Vec<u8>),
    /// An ECDSA P-256 public key (SEC1 uncompressed, 65 bytes).
    EcPublic(Vec<u8>),
    /// An Ed25519 private seed, with its public key.
    EdPrivate([u8; 32], [u8; 32]),
    /// An Ed25519 public key.
    EdPublic([u8; 32]),
}

fn unsupported(op: &str, name: &str) -> Error {
    Error::dom(
        "NotSupportedError",
        format!(
            "Failed to execute '{op}' on 'SubtleCrypto': Algorithm: {name} is not supported by pageon (SHA-1/256/384/512, HMAC, ECDSA P-256, Ed25519, AES-GCM)."
        ),
    )
}

fn op_error(op: &str, why: &str) -> Error {
    Error::dom(
        "OperationError",
        format!("Failed to execute '{op}' on 'SubtleCrypto': {why}"),
    )
}

fn access(op: &str, why: &str) -> Error {
    Error::dom(
        "InvalidAccessError",
        format!("Failed to execute '{op}' on 'SubtleCrypto': {why}"),
    )
}

/// A JS value as JSON (an algorithm dictionary or a JWK).
fn to_json(cx: &mut Cx<'_, '_, '_>, v: &Value) -> R<Json> {
    let l = cx.local(v);
    if l.is_string() {
        return Ok(json!({"name": crate::cv::string(cx, l)?}));
    }
    // Typed arrays in algorithm parameters (iv, additionalData) are read separately.
    let s = v8::json::stringify(cx.scope, l)
        .map(|s| s.to_rust_string_lossy(cx.scope))
        .unwrap_or_default();
    serde_json::from_str(&s)
        .map_err(|_| Error::Type("The algorithm or key is not an object.".into()))
}

fn from_json(cx: &mut Cx<'_, '_, '_>, j: &Json) -> R<Value> {
    let s = cx.str(&j.to_string());
    let v = v8::json::parse(cx.scope, s).ok_or(Error::Terminated)?;
    Ok(cx.keep(v))
}

/// A named member of an algorithm object, as a JS value (for buffer parameters).
fn member(cx: &mut Cx<'_, '_, '_>, v: &Value, name: &str) -> Option<Value> {
    let l = cx.local(v);
    let o = v8::Local::<v8::Object>::try_from(l).ok()?;
    let key = cx.str(name);
    let m = o.get(cx.scope, key.into())?;
    (!m.is_undefined()).then(|| cx.keep(m))
}

/// Bytes of a `BufferSource`.
fn bytes(cx: &mut Cx<'_, '_, '_>, v: &Value) -> R<Vec<u8>> {
    let l = cx.local(v);
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(l) {
        let mut buf = vec![0; view.byte_length()];
        view.copy_contents(&mut buf);
        return Ok(buf);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(l) {
        let len = ab.byte_length();
        let store = ab.get_backing_store();
        return Ok(store.iter().take(len).map(std::cell::Cell::get).collect());
    }
    Err(Error::Type(
        "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.".into(),
    ))
}

fn name_of(j: &Json) -> String {
    j.get("name")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// The canonical algorithm name, or `None` if pageon does not support it.
fn canonical(name: &str) -> Option<&'static str> {
    Some(match name {
        "SHA-1" => "SHA-1",
        "SHA-256" => "SHA-256",
        "SHA-384" => "SHA-384",
        "SHA-512" => "SHA-512",
        "HMAC" => "HMAC",
        "ECDSA" => "ECDSA",
        "ED25519" => "Ed25519",
        "AES-GCM" => "AES-GCM",
        _ => return None,
    })
}

fn hash_of(j: &Json, op: &str) -> R<&'static str> {
    let h = match j.get("hash") {
        Some(Json::String(s)) => s.to_ascii_uppercase(),
        Some(o) => name_of(o),
        None => {
            return Err(Error::Type(format!(
                "Failed to execute '{op}' on 'SubtleCrypto': Algorithm: hash is missing."
            )));
        }
    };
    match canonical(&h) {
        Some(c) if c.starts_with("SHA-") => Ok(c),
        _ => Err(unsupported(op, &h)),
    }
}

fn digest_alg(h: &str) -> &'static digest::Algorithm {
    match h {
        "SHA-1" => &digest::SHA1_FOR_LEGACY_USE_ONLY,
        "SHA-384" => &digest::SHA384,
        "SHA-512" => &digest::SHA512,
        _ => &digest::SHA256,
    }
}

fn hmac_alg(h: &str) -> hmac::Algorithm {
    match h {
        "SHA-1" => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        "SHA-384" => hmac::HMAC_SHA384,
        "SHA-512" => hmac::HMAC_SHA512,
        _ => hmac::HMAC_SHA256,
    }
}

fn block_bits(h: &str) -> usize {
    match h {
        "SHA-384" | "SHA-512" => 1024,
        _ => 512,
    }
}

fn random(cx: &Cx<'_, '_, '_>, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    cx.st().crypto.fill(&mut b);
    b
}

fn key_of(cx: &Cx<'_, '_, '_>, key: Obj) -> R<Key> {
    match cx.st().slot(key.slab()?) {
        Some(Slot::CryptoKey(k)) => Ok((**k).clone()),
        _ => Err(Error::Type(
            "The provided value is not of type 'CryptoKey'.".into(),
        )),
    }
}

fn new_key(cx: &mut Cx<'_, '_, '_>, k: Key) -> Obj {
    cx.alloc(Iface::CryptoKey, Slot::CryptoKey(Box::new(k)))
}

fn check_usages(usages: &[String], allowed: &[&str], op: &str) -> R<()> {
    match usages.iter().find(|u| !allowed.contains(&u.as_str())) {
        Some(u) => Err(Error::dom(
            "SyntaxError",
            format!(
                "Failed to execute '{op}' on 'SubtleCrypto': Cannot create a key with usage '{u}'."
            ),
        )),
        None => Ok(()),
    }
}

fn uses(k: &Key, usage: &str, op: &str) -> R<()> {
    if k.usages.iter().any(|u| u == usage) {
        Ok(())
    } else {
        Err(access(
            op,
            &format!("key.usages does not permit this operation ({usage})."),
        ))
    }
}

/// Settles a promise with an `ArrayBuffer` of `r`'s bytes, or its error.
fn settle_bytes(cx: &mut Cx<'_, '_, '_>, r: R<Vec<u8>>) -> R<Value> {
    match r {
        Ok(b) => {
            let v = crate::dom::blobs::array_buffer(cx, &b);
            let l = cx.local(&v);
            crate::dom::net::settled(cx, Ok(l))
        }
        Err(Error::Terminated) => Err(Error::Terminated),
        Err(e) => crate::dom::net::settled(cx, Err(e)),
    }
}

fn settle_value(cx: &mut Cx<'_, '_, '_>, r: R<Value>) -> R<Value> {
    match r {
        Ok(v) => {
            let l = cx.local(&v);
            crate::dom::net::settled(cx, Ok(l))
        }
        Err(Error::Terminated) => Err(Error::Terminated),
        Err(e) => crate::dom::net::settled(cx, Err(e)),
    }
}

// ----- ECDSA P-256 and Ed25519 helpers -----

fn ec_public_of(scalar: &[u8]) -> R<Vec<u8>> {
    let sk = p256::ecdsa::SigningKey::from_slice(scalar)
        .map_err(|_| op_error("importKey", "invalid P-256 private key."))?;
    Ok(sk.verifying_key().to_sec1_point(false).as_bytes().to_vec())
}

fn ed_public_of(seed: &[u8; 32]) -> R<[u8; 32]> {
    let kp = signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| op_error("importKey", "invalid Ed25519 key."))?;
    let mut out = [0u8; 32];
    out.copy_from_slice(signature::KeyPair::public_key(&kp).as_ref());
    Ok(out)
}

/// PKCS#8 (RFC 8410) for an Ed25519 seed.
const ED_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
/// SubjectPublicKeyInfo (RFC 8410) for an Ed25519 public key.
const ED_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

// ----- operations -----

pub mod subtle_crypto {
    use super::*;

    pub fn digest(cx: &mut Cx<'_, '_, '_>, _this: Obj, algorithm: Value, data: Value) -> R<Value> {
        let r = (|| {
            let a = to_json(cx, &algorithm)?;
            let n = name_of(&a);
            let h = canonical(&n)
                .filter(|c| c.starts_with("SHA-"))
                .ok_or_else(|| unsupported("digest", &n))?;
            let d = bytes(cx, &data)?;
            Ok(digest::digest(digest_alg(h), &d).as_ref().to_vec())
        })();
        settle_bytes(cx, r)
    }

    pub fn generate_key(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        algorithm: Value,
        extractable: bool,
        usages: Vec<String>,
    ) -> R<Value> {
        let r = (|| -> R<Value> {
            let a = to_json(cx, &algorithm)?;
            let n = name_of(&a);
            match canonical(&n) {
                Some("HMAC") => {
                    check_usages(&usages, &["sign", "verify"], "generateKey")?;
                    let h = hash_of(&a, "generateKey")?;
                    let bits = a
                        .get("length")
                        .and_then(Json::as_u64)
                        .map_or(block_bits(h), |l| l as usize);
                    if bits == 0 || bits % 8 != 0 {
                        return Err(op_error(
                            "generateKey",
                            "HMAC key length must be a positive multiple of 8.",
                        ));
                    }
                    let k = Key {
                        kind: "secret",
                        algorithm: json!({"name": "HMAC", "hash": {"name": h}, "length": bits}),
                        usages,
                        extractable,
                        material: Material::Secret(random(cx, bits / 8)),
                    };
                    let o = new_key(cx, k);
                    let w: v8::Local<v8::Value> = cx.wrap(o).into();
                    Ok(cx.keep(w))
                }
                Some("AES-GCM") => {
                    check_usages(
                        &usages,
                        &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
                        "generateKey",
                    )?;
                    let bits = a.get("length").and_then(Json::as_u64).unwrap_or(0) as usize;
                    if !matches!(bits, 128 | 256) {
                        return Err(op_error(
                            "generateKey",
                            "AES key length must be 128 or 256 bits.",
                        ));
                    }
                    let k = Key {
                        kind: "secret",
                        algorithm: json!({"name": "AES-GCM", "length": bits}),
                        usages,
                        extractable,
                        material: Material::Secret(random(cx, bits / 8)),
                    };
                    let o = new_key(cx, k);
                    let w: v8::Local<v8::Value> = cx.wrap(o).into();
                    Ok(cx.keep(w))
                }
                Some("ECDSA") => {
                    check_usages(&usages, &["sign", "verify"], "generateKey")?;
                    let curve = a
                        .get("namedCurve")
                        .and_then(Json::as_str)
                        .unwrap_or_default();
                    if curve != "P-256" {
                        return Err(unsupported("generateKey", &format!("ECDSA {curve}")));
                    }
                    // A scalar from the crypto stream (valid with overwhelming probability).
                    let mut scalar = random(cx, 32);
                    while p256::ecdsa::SigningKey::from_slice(&scalar).is_err() {
                        scalar = random(cx, 32);
                    }
                    let public = ec_public_of(&scalar)?;
                    let alg = json!({"name": "ECDSA", "namedCurve": "P-256"});
                    pair(
                        cx,
                        alg,
                        usages,
                        extractable,
                        Material::EcPrivate(scalar, public.clone()),
                        Material::EcPublic(public),
                    )
                }
                Some("Ed25519") => {
                    check_usages(&usages, &["sign", "verify"], "generateKey")?;
                    let mut seed = [0u8; 32];
                    seed.copy_from_slice(&random(cx, 32));
                    let public = ed_public_of(&seed)?;
                    let alg = json!({"name": "Ed25519"});
                    pair(
                        cx,
                        alg,
                        usages,
                        extractable,
                        Material::EdPrivate(seed, public),
                        Material::EdPublic(public),
                    )
                }
                _ => Err(unsupported("generateKey", &n)),
            }
        })();
        settle_value(cx, r)
    }

    /// A `CryptoKeyPair`: the public key (always extractable, `verify`) and the private key
    /// (`sign`).
    fn pair(
        cx: &mut Cx<'_, '_, '_>,
        alg: Json,
        usages: Vec<String>,
        extractable: bool,
        private: Material,
        public: Material,
    ) -> R<Value> {
        let has = |u: &str| usages.iter().any(|x| x == u);
        if !has("sign") && !has("verify") {
            return Err(Error::dom(
                "SyntaxError",
                "Failed to execute 'generateKey' on 'SubtleCrypto': Usages cannot be empty when creating a key.",
            ));
        }
        let pk = new_key(
            cx,
            Key {
                kind: "public",
                algorithm: alg.clone(),
                usages: if has("verify") {
                    vec!["verify".into()]
                } else {
                    Vec::new()
                },
                extractable: true,
                material: public,
            },
        );
        let sk = new_key(
            cx,
            Key {
                kind: "private",
                algorithm: alg,
                usages: if has("sign") {
                    vec!["sign".into()]
                } else {
                    Vec::new()
                },
                extractable,
                material: private,
            },
        );
        let o = v8::Object::new(cx.scope);
        let (pk, sk): (v8::Local<v8::Value>, v8::Local<v8::Value>) =
            (cx.wrap(pk).into(), cx.wrap(sk).into());
        let kp = cx.str("publicKey");
        o.set(cx.scope, kp.into(), pk);
        let ks = cx.str("privateKey");
        o.set(cx.scope, ks.into(), sk);
        Ok(cx.keep(o.into()))
    }

    pub fn import_key(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        format: String,
        data: Value,
        algorithm: Value,
        extractable: bool,
        usages: Vec<String>,
    ) -> R<Value> {
        let r = (|| -> R<Value> {
            let a = to_json(cx, &algorithm)?;
            let n = name_of(&a);
            let jwk = if format == "jwk" {
                Some(to_json(cx, &data)?)
            } else {
                None
            };
            let raw = if format == "jwk" {
                Vec::new()
            } else {
                bytes(cx, &data)?
            };
            let b64 = |j: &Json, f: &str| -> R<Vec<u8>> {
                j.get(f)
                    .and_then(Json::as_str)
                    .and_then(|s| B64.decode(s).ok())
                    .ok_or_else(|| Error::dom("DataError", format!("Failed to execute 'importKey' on 'SubtleCrypto': The JWK member \"{f}\" is missing or not base64url.")))
            };
            let key = match canonical(&n) {
                Some(alg @ ("HMAC" | "AES-GCM")) => {
                    let bytes = match (format.as_str(), &jwk) {
                        ("raw", _) => raw,
                        ("jwk", Some(j)) => b64(j, "k")?,
                        _ => return Err(unsupported("importKey", &format!("{n} {format}"))),
                    };
                    if alg == "HMAC" {
                        check_usages(&usages, &["sign", "verify"], "importKey")?;
                        let h = hash_of(&a, "importKey")?;
                        Key {
                            kind: "secret",
                            algorithm: json!({"name": "HMAC", "hash": {"name": h}, "length": bytes.len() * 8}),
                            usages,
                            extractable,
                            material: Material::Secret(bytes),
                        }
                    } else {
                        check_usages(
                            &usages,
                            &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
                            "importKey",
                        )?;
                        if !matches!(bytes.len(), 16 | 32) {
                            return Err(Error::dom(
                                "DataError",
                                "Failed to execute 'importKey' on 'SubtleCrypto': AES key data must be 128 or 256 bits.",
                            ));
                        }
                        Key {
                            kind: "secret",
                            algorithm: json!({"name": "AES-GCM", "length": bytes.len() * 8}),
                            usages,
                            extractable,
                            material: Material::Secret(bytes),
                        }
                    }
                }
                Some("ECDSA") => {
                    let curve = a
                        .get("namedCurve")
                        .and_then(Json::as_str)
                        .unwrap_or_default();
                    if curve != "P-256" {
                        return Err(unsupported("importKey", &format!("ECDSA {curve}")));
                    }
                    let alg = json!({"name": "ECDSA", "namedCurve": "P-256"});
                    let bad = || {
                        Error::dom(
                            "DataError",
                            "Failed to execute 'importKey' on 'SubtleCrypto': invalid P-256 key data.",
                        )
                    };
                    let private = |scalar: Vec<u8>| -> R<Key> {
                        check_usages(&usages, &["sign"], "importKey")?;
                        let public = ec_public_of(&scalar).map_err(|_| bad())?;
                        Ok(Key {
                            kind: "private",
                            algorithm: alg.clone(),
                            usages: usages.clone(),
                            extractable,
                            material: Material::EcPrivate(scalar, public),
                        })
                    };
                    let public = |sec1: Vec<u8>| -> R<Key> {
                        check_usages(&usages, &["verify"], "importKey")?;
                        p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| bad())?;
                        Ok(Key {
                            kind: "public",
                            algorithm: alg.clone(),
                            usages: usages.clone(),
                            extractable: true,
                            material: Material::EcPublic(sec1),
                        })
                    };
                    match (format.as_str(), &jwk) {
                        ("raw", _) => public(raw)?,
                        ("spki", _) => {
                            use p256::pkcs8::DecodePublicKey as _;
                            let vk = p256::ecdsa::VerifyingKey::from_public_key_der(&raw)
                                .map_err(|_| bad())?;
                            public(vk.to_sec1_point(false).as_bytes().to_vec())?
                        }
                        ("pkcs8", _) => {
                            use p256::pkcs8::DecodePrivateKey as _;
                            let sk =
                                p256::ecdsa::SigningKey::from_pkcs8_der(&raw).map_err(|_| bad())?;
                            private(sk.to_bytes().to_vec())?
                        }
                        ("jwk", Some(j)) => {
                            if j.get("kty").and_then(Json::as_str) != Some("EC")
                                || j.get("crv").and_then(Json::as_str) != Some("P-256")
                            {
                                return Err(bad());
                            }
                            if j.get("d").is_some() {
                                private(b64(j, "d")?)?
                            } else {
                                let (x, y) = (b64(j, "x")?, b64(j, "y")?);
                                let mut sec1 = vec![4u8];
                                sec1.extend(x);
                                sec1.extend(y);
                                public(sec1)?
                            }
                        }
                        _ => return Err(unsupported("importKey", &format!("ECDSA {format}"))),
                    }
                }
                Some("Ed25519") => {
                    let alg = json!({"name": "Ed25519"});
                    let bad = || {
                        Error::dom(
                            "DataError",
                            "Failed to execute 'importKey' on 'SubtleCrypto': invalid Ed25519 key data.",
                        )
                    };
                    let arr = |b: &[u8]| -> R<[u8; 32]> { b.try_into().map_err(|_| bad()) };
                    let private = |seed: [u8; 32]| -> R<Key> {
                        check_usages(&usages, &["sign"], "importKey")?;
                        let public = ed_public_of(&seed).map_err(|_| bad())?;
                        Ok(Key {
                            kind: "private",
                            algorithm: alg.clone(),
                            usages: usages.clone(),
                            extractable,
                            material: Material::EdPrivate(seed, public),
                        })
                    };
                    let public = |p: [u8; 32]| -> R<Key> {
                        check_usages(&usages, &["verify"], "importKey")?;
                        Ok(Key {
                            kind: "public",
                            algorithm: alg.clone(),
                            usages: usages.clone(),
                            extractable: true,
                            material: Material::EdPublic(p),
                        })
                    };
                    match (format.as_str(), &jwk) {
                        ("raw", _) => public(arr(&raw)?)?,
                        ("spki", _) if raw.len() == 44 && raw[..12] == ED_SPKI_PREFIX => {
                            public(arr(&raw[12..])?)?
                        }
                        ("pkcs8", _) if raw.len() == 48 && raw[..16] == ED_PKCS8_PREFIX => {
                            private(arr(&raw[16..])?)?
                        }
                        ("jwk", Some(j)) => {
                            if j.get("kty").and_then(Json::as_str) != Some("OKP")
                                || j.get("crv").and_then(Json::as_str) != Some("Ed25519")
                            {
                                return Err(bad());
                            }
                            if j.get("d").is_some() {
                                private(arr(&b64(j, "d")?)?)?
                            } else {
                                public(arr(&b64(j, "x")?)?)?
                            }
                        }
                        ("spki" | "pkcs8", _) => return Err(bad()),
                        _ => return Err(unsupported("importKey", &format!("Ed25519 {format}"))),
                    }
                }
                _ => return Err(unsupported("importKey", &n)),
            };
            let o = new_key(cx, key);
            let w: v8::Local<v8::Value> = cx.wrap(o).into();
            Ok(cx.keep(w))
        })();
        settle_value(cx, r)
    }

    pub fn export_key(cx: &mut Cx<'_, '_, '_>, _this: Obj, format: String, key: Obj) -> R<Value> {
        let r = (|| -> R<Value> {
            let k = key_of(cx, key)?;
            if !k.extractable {
                return Err(access("exportKey", "key is not extractable."));
            }
            let name = k
                .algorithm
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            let out: Result<Vec<u8>, Json> = match (&k.material, format.as_str()) {
                (Material::Secret(b), "raw") => Ok(b.clone()),
                (Material::Secret(b), "jwk") => {
                    let alg = match (name.as_str(), &k.algorithm) {
                        ("HMAC", a) => format!(
                            "HS{}",
                            a["hash"]["name"]
                                .as_str()
                                .unwrap_or("SHA-256")
                                .trim_start_matches("SHA-")
                        ),
                        (_, a) => format!("A{}GCM", a["length"].as_u64().unwrap_or(256)),
                    };
                    Err(
                        json!({"kty": "oct", "k": B64.encode(b), "alg": alg, "key_ops": k.usages, "ext": true}),
                    )
                }
                (Material::EcPublic(p) | Material::EcPrivate(_, p), "raw")
                    if k.kind == "public" =>
                {
                    Ok(p.clone())
                }
                (Material::EcPublic(p), "spki") => {
                    use p256::pkcs8::EncodePublicKey as _;
                    let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(p)
                        .map_err(|_| op_error("exportKey", "invalid key."))?;
                    Ok(vk
                        .to_public_key_der()
                        .map_err(|_| op_error("exportKey", "invalid key."))?
                        .as_bytes()
                        .to_vec())
                }
                (Material::EcPrivate(s, _), "pkcs8") => {
                    use p256::pkcs8::EncodePrivateKey as _;
                    let sk = p256::ecdsa::SigningKey::from_slice(s)
                        .map_err(|_| op_error("exportKey", "invalid key."))?;
                    Ok(sk
                        .to_pkcs8_der()
                        .map_err(|_| op_error("exportKey", "invalid key."))?
                        .as_bytes()
                        .to_vec())
                }
                (Material::EcPublic(p), "jwk") => Err(
                    json!({"kty": "EC", "crv": "P-256", "x": B64.encode(&p[1..33]), "y": B64.encode(&p[33..65]), "key_ops": k.usages, "ext": true}),
                ),
                (Material::EcPrivate(s, p), "jwk") => Err(
                    json!({"kty": "EC", "crv": "P-256", "x": B64.encode(&p[1..33]), "y": B64.encode(&p[33..65]), "d": B64.encode(s), "key_ops": k.usages, "ext": true}),
                ),
                (Material::EdPublic(p), "raw") => Ok(p.to_vec()),
                (Material::EdPublic(p), "spki") => Ok([&ED_SPKI_PREFIX[..], p].concat()),
                (Material::EdPrivate(s, _), "pkcs8") => Ok([&ED_PKCS8_PREFIX[..], s].concat()),
                (Material::EdPublic(p), "jwk") => Err(
                    json!({"kty": "OKP", "crv": "Ed25519", "x": B64.encode(p), "key_ops": k.usages, "ext": true}),
                ),
                (Material::EdPrivate(s, p), "jwk") => Err(
                    json!({"kty": "OKP", "crv": "Ed25519", "x": B64.encode(p), "d": B64.encode(s), "key_ops": k.usages, "ext": true}),
                ),
                _ => {
                    return Err(Error::dom(
                        "InvalidAccessError",
                        format!(
                            "Failed to execute 'exportKey' on 'SubtleCrypto': a {} {name} key cannot be exported as {format}.",
                            k.kind
                        ),
                    ));
                }
            };
            match out {
                Ok(b) => {
                    let v = crate::dom::blobs::array_buffer(cx, &b);
                    Ok(v)
                }
                Err(j) => from_json(cx, &j),
            }
        })();
        settle_value(cx, r)
    }

    pub fn sign(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        algorithm: Value,
        key: Obj,
        data: Value,
    ) -> R<Value> {
        let r = (|| -> R<Vec<u8>> {
            let a = to_json(cx, &algorithm)?;
            let k = key_of(cx, key)?;
            uses(&k, "sign", "sign")?;
            let d = bytes(cx, &data)?;
            match (&k.material, canonical(&name_of(&a))) {
                (Material::Secret(s), Some("HMAC")) => {
                    let h = k.algorithm["hash"]["name"].as_str().unwrap_or("SHA-256");
                    Ok(hmac::sign(&hmac::Key::new(hmac_alg(h), s), &d)
                        .as_ref()
                        .to_vec())
                }
                (Material::EcPrivate(s, _), Some("ECDSA")) => {
                    use p256::ecdsa::signature::hazmat::PrehashSigner as _;
                    let h = hash_of(&a, "sign")?;
                    let prehash = digest::digest(digest_alg(h), &d);
                    let sk = p256::ecdsa::SigningKey::from_slice(s)
                        .map_err(|_| op_error("sign", "invalid key."))?;
                    // RFC 6979: deterministic, so a replay sends the same signature.
                    let sig: p256::ecdsa::Signature = sk
                        .sign_prehash(prehash.as_ref())
                        .map_err(|_| op_error("sign", "signing failed."))?;
                    Ok(sig.to_bytes().to_vec())
                }
                (Material::EdPrivate(s, _), Some("Ed25519")) => {
                    let kp = signature::Ed25519KeyPair::from_seed_unchecked(s)
                        .map_err(|_| op_error("sign", "invalid key."))?;
                    Ok(kp.sign(&d).as_ref().to_vec())
                }
                (_, Some(n)) => Err(access(
                    "sign",
                    &format!("the key is not a {n} signing key."),
                )),
                (_, None) => Err(unsupported("sign", &name_of(&a))),
            }
        })();
        settle_bytes(cx, r)
    }

    pub fn verify(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        algorithm: Value,
        key: Obj,
        sig: Value,
        data: Value,
    ) -> R<Value> {
        let r = (|| -> R<bool> {
            let a = to_json(cx, &algorithm)?;
            let k = key_of(cx, key)?;
            uses(&k, "verify", "verify")?;
            let s = bytes(cx, &sig)?;
            let d = bytes(cx, &data)?;
            Ok(match (&k.material, canonical(&name_of(&a))) {
                (Material::Secret(key), Some("HMAC")) => {
                    let h = k.algorithm["hash"]["name"].as_str().unwrap_or("SHA-256");
                    hmac::verify(&hmac::Key::new(hmac_alg(h), key), &d, &s).is_ok()
                }
                (Material::EcPublic(p), Some("ECDSA")) => {
                    use p256::ecdsa::signature::hazmat::PrehashVerifier as _;
                    let h = hash_of(&a, "verify")?;
                    let prehash = digest::digest(digest_alg(h), &d);
                    match (
                        p256::ecdsa::VerifyingKey::from_sec1_bytes(p),
                        p256::ecdsa::Signature::from_slice(&s),
                    ) {
                        (Ok(vk), Ok(sig)) => vk.verify_prehash(prehash.as_ref(), &sig).is_ok(),
                        _ => false,
                    }
                }
                (Material::EdPublic(p), Some("Ed25519")) => {
                    signature::UnparsedPublicKey::new(&signature::ED25519, p)
                        .verify(&d, &s)
                        .is_ok()
                }
                (_, Some(n)) => {
                    return Err(access(
                        "verify",
                        &format!("the key is not a {n} verification key."),
                    ));
                }
                (_, None) => return Err(unsupported("verify", &name_of(&a))),
            })
        })();
        match r {
            Ok(b) => {
                let v: v8::Local<v8::Value> = v8::Boolean::new(cx.scope, b).into();
                crate::dom::net::settled(cx, Ok(v))
            }
            Err(Error::Terminated) => Err(Error::Terminated),
            Err(e) => crate::dom::net::settled(cx, Err(e)),
        }
    }

    fn aes_gcm(
        cx: &mut Cx<'_, '_, '_>,
        op: &str,
        algorithm: &Value,
        key: Obj,
        data: &Value,
    ) -> R<Vec<u8>> {
        let a = to_json(cx, algorithm)?;
        let n = name_of(&a);
        if canonical(&n) != Some("AES-GCM") {
            return Err(unsupported(op, &n));
        }
        let k = key_of(cx, key)?;
        uses(&k, op, op)?;
        let Material::Secret(secret) = &k.material else {
            return Err(access(op, "the key is not an AES-GCM key."));
        };
        if k.algorithm["name"] != "AES-GCM" {
            return Err(access(op, "the key is not an AES-GCM key."));
        }
        let iv = member(cx, algorithm, "iv").ok_or_else(|| {
            Error::Type(format!(
                "Failed to execute '{op}' on 'SubtleCrypto': AesGcmParams: iv is missing."
            ))
        })?;
        let iv = bytes(cx, &iv)?;
        let aad = match member(cx, algorithm, "additionalData") {
            Some(v) => bytes(cx, &v)?,
            None => Vec::new(),
        };
        if a.get("tagLength")
            .and_then(Json::as_u64)
            .is_some_and(|t| t != 128)
        {
            return Err(op_error(op, "pageon supports 128-bit AES-GCM tags."));
        }
        let nonce = aead::Nonce::try_assume_unique_for_key(&iv)
            .map_err(|_| op_error(op, "pageon supports 96-bit (12-byte) AES-GCM IVs."))?;
        let alg = if secret.len() == 16 {
            &aead::AES_128_GCM
        } else {
            &aead::AES_256_GCM
        };
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(alg, secret).map_err(|_| op_error(op, "invalid key."))?,
        );
        let mut buf = bytes(cx, data)?;
        if op == "encrypt" {
            key.seal_in_place_append_tag(nonce, aead::Aad::from(&aad), &mut buf)
                .map_err(|_| op_error(op, "encryption failed."))?;
            Ok(buf)
        } else {
            let plain = key
                .open_in_place(nonce, aead::Aad::from(&aad), &mut buf)
                .map_err(|_| {
                    op_error(op, "The operation failed for an operation-specific reason.")
                })?;
            Ok(plain.to_vec())
        }
    }

    pub fn encrypt(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        algorithm: Value,
        key: Obj,
        data: Value,
    ) -> R<Value> {
        let r = aes_gcm(cx, "encrypt", &algorithm, key, &data);
        settle_bytes(cx, r)
    }

    pub fn decrypt(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        algorithm: Value,
        key: Obj,
        data: Value,
    ) -> R<Value> {
        let r = aes_gcm(cx, "decrypt", &algorithm, key, &data);
        settle_bytes(cx, r)
    }
}

pub mod crypto_key {
    use super::*;

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(key_of(cx, this)?.kind.to_string())
    }

    pub fn get_extractable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(key_of(cx, this)?.extractable)
    }

    pub fn get_algorithm(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let a = key_of(cx, this)?.algorithm;
        from_json(cx, &a)
    }

    pub fn get_usages(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let u = Json::from(key_of(cx, this)?.usages);
        from_json(cx, &u)
    }
}
