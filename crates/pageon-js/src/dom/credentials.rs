//! WebAuthn on the page side: `navigator.credentials` asks the environment (in a sandbox, the
//! broker) to create or use a passkey, and wraps the public answer as a `PublicKeyCredential`.
//! The client data is built here; the authenticator data is built and signed where the key is.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pageon_env::{NetError, PasskeyCall, PasskeyReply};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

/// A passkey call waiting for the environment, with its promise.
pub struct Waiting {
    /// The realm that asked.
    pub realm: usize,
    /// What to ask.
    pub call: PasskeyCall,
    /// The promise to settle.
    pub resolver: v8::Global<v8::PromiseResolver>,
    /// The client data JSON (for the credential's response).
    pub client_data: Vec<u8>,
}

impl std::fmt::Debug for Waiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiting")
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}

/// A `PublicKeyCredential`.
#[derive(Debug, Clone)]
pub struct Cred {
    id: Vec<u8>,
    raw_id: Value,
    response: u32,
}

/// An `AuthenticatorAttestationResponse` or `AuthenticatorAssertionResponse`.
#[derive(Debug, Clone)]
pub struct Resp {
    client_data: Value,
    authenticator_data: Vec<u8>,
    /// Attestation: the object and the SPKI public key.
    attestation: Option<(Value, Vec<u8>)>,
    /// Assertion: the authenticator data object, the signature and the user handle.
    assertion: Option<(Value, Value, Option<Value>)>,
}

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

fn cred(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Cred> {
    match this {
        Obj::Slab(id) => match cx.st().slot(id) {
            Some(Slot::Credential(c)) => Ok((**c).clone()),
            _ => Err(illegal()),
        },
        _ => Err(illegal()),
    }
}

fn resp(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Resp> {
    match this {
        Obj::Slab(id) => match cx.st().slot(id) {
            Some(Slot::AuthResponse(r)) => Ok((**r).clone()),
            _ => Err(illegal()),
        },
        _ => Err(illegal()),
    }
}

fn null(cx: &mut Cx<'_, '_, '_>) -> Value {
    let n = v8::null(cx.scope).into();
    cx.keep(n)
}

fn resolved(cx: &mut Cx<'_, '_, '_>, b: bool) -> R<Value> {
    let v = v8::Boolean::new(cx.scope, b).into();
    crate::dom::net::settled(cx, Ok(v))
}

fn origin(cx: &Cx<'_, '_, '_>) -> String {
    cx.st().origin.clone()
}

fn host_of(origin: &str) -> String {
    url::Url::parse(origin)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn bytes(v: &serde_json::Value, what: &str) -> R<Vec<u8>> {
    v.as_str()
        .and_then(|s| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok())
        .ok_or_else(|| Error::Type(format!("{what} must be a BufferSource")))
}

fn ids(v: &serde_json::Value) -> R<Vec<Vec<u8>>> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|d| bytes(&d["id"], "a credential id"))
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"{kind}","challenge":"{}","origin":{},"crossOrigin":false}}"#,
        URL_SAFE_NO_PAD.encode(challenge),
        serde_json::Value::String(origin.to_string())
    )
    .into_bytes()
}

fn sha256(b: &[u8]) -> Vec<u8> {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, b)
        .as_ref()
        .to_vec()
}

/// `__pageon.passkey(op, json)` (from the prelude): checks the request, builds the client data
/// and queues the call; returns its promise.
pub(crate) fn host_call(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let op = crate::cv::string(cx, args.get(0))?;
        let json = crate::cv::string(cx, args.get(1))?;
        let o: serde_json::Value =
            serde_json::from_str(&json).map_err(|e| Error::Type(format!("bad options: {e}")))?;
        let (resolver, promise) = crate::dom::net::promise(cx)?;
        let origin = origin(cx);
        let queued = (|| -> R<Option<(PasskeyCall, Vec<u8>)>> {
            if op == "available" {
                return Ok(Some((PasskeyCall::Available, Vec::new())));
            }
            // A conditional (autofill) request waits for a picker pageon does not have.
            if o["mediation"] == "conditional" {
                return Ok(None);
            }
            if !crate::dom::editing::activated(cx) {
                return Err(Error::dom(
                    "NotAllowedError",
                    "A passkey is used only after a click or key (the agent's action).",
                ));
            }
            let challenge = bytes(&o["challenge"], "challenge")?;
            if op == "create" {
                let algs = o["pubKeyCredParams"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if !algs.iter().any(|p| p["alg"] == -7) {
                    return Err(Error::dom(
                        "NotSupportedError",
                        "pageon's authenticator signs with ES256 (-7) only.",
                    ));
                }
                let rp_id = o["rp"]["id"]
                    .as_str()
                    .map_or_else(|| host_of(&origin), str::to_string);
                let cd = client_data("webauthn.create", &challenge, &origin);
                let call = PasskeyCall::Create {
                    origin: origin.clone(),
                    rp_id,
                    user_id: bytes(&o["user"]["id"], "user.id")?,
                    user_name: o["user"]["name"].as_str().unwrap_or_default().to_string(),
                    exclude: ids(&o["excludeCredentials"])?,
                    client_data_hash: sha256(&cd),
                };
                return Ok(Some((call, cd)));
            }
            let rp_id = o["rpId"]
                .as_str()
                .map_or_else(|| host_of(&origin), str::to_string);
            let cd = client_data("webauthn.get", &challenge, &origin);
            let call = PasskeyCall::Get {
                origin: origin.clone(),
                rp_id,
                allow: ids(&o["allowCredentials"])?,
                client_data_hash: sha256(&cd),
            };
            Ok(Some((call, cd)))
        })();
        match queued {
            Ok(Some((call, client_data))) => {
                let realm = cx.realm.id;
                cx.realm
                    .shared
                    .borrow_mut()
                    .passkey_calls
                    .push_back(Waiting {
                        realm,
                        call,
                        resolver,
                        client_data,
                    });
            }
            Ok(None) => {}
            Err(e) => {
                let v = crate::cv::error_value(cx, e).ok_or(Error::Terminated)?;
                let r = v8::Local::new(cx.scope, &resolver);
                r.reject(cx.scope, v);
            }
        }
        Ok(crate::cv::Out::Value(promise))
    });
}

/// Settles a passkey call with the environment's answer.
pub(crate) fn answer(
    cx: &mut Cx<'_, '_, '_>,
    w: Waiting,
    reply: Result<PasskeyReply, NetError>,
) -> R<()> {
    // Without a store, no authenticator is available (rather than an error).
    let reply = match (&w.call, reply) {
        (PasskeyCall::Available, Err(_)) => Ok(PasskeyReply::Available(false)),
        (_, r) => r,
    };
    let result: R<v8::Local<v8::Value>> = match reply {
        Ok(PasskeyReply::Available(b)) => Ok(v8::Boolean::new(cx.scope, b).into()),
        Ok(PasskeyReply::Refused { name, message }) => Err(Error::Dom(dom_name(&name), message)),
        Err(e) => Err(Error::dom("NotAllowedError", e.to_string())),
        Ok(PasskeyReply::Created {
            id,
            authenticator_data,
            attestation_object,
            public_key,
        }) => {
            let client_data = crate::dom::blobs::array_buffer(cx, &w.client_data);
            let att = crate::dom::blobs::array_buffer(cx, &attestation_object);
            let r = Resp {
                client_data,
                authenticator_data,
                attestation: Some((att, public_key)),
                assertion: None,
            };
            let o = credential(cx, id, r, Iface::AuthenticatorAttestationResponse);
            Ok(cx.wrap(o).into())
        }
        Ok(PasskeyReply::Signed {
            id,
            authenticator_data,
            signature,
            user_handle,
        }) => {
            let client_data = crate::dom::blobs::array_buffer(cx, &w.client_data);
            let ad = crate::dom::blobs::array_buffer(cx, &authenticator_data);
            let sig = crate::dom::blobs::array_buffer(cx, &signature);
            let uh = (!user_handle.is_empty())
                .then(|| crate::dom::blobs::array_buffer(cx, &user_handle));
            let r = Resp {
                client_data,
                authenticator_data,
                attestation: None,
                assertion: Some((ad, sig, uh)),
            };
            let o = credential(cx, id, r, Iface::AuthenticatorAssertionResponse);
            Ok(cx.wrap(o).into())
        }
    };
    let r = v8::Local::new(cx.scope, &w.resolver);
    match result {
        Ok(v) => {
            r.resolve(cx.scope, v);
        }
        Err(e) => {
            if let Some(v) = crate::cv::error_value(cx, e) {
                r.reject(cx.scope, v);
            }
        }
    }
    Ok(())
}

fn dom_name(name: &str) -> &'static str {
    match name {
        "SecurityError" => "SecurityError",
        "InvalidStateError" => "InvalidStateError",
        "NotSupportedError" => "NotSupportedError",
        _ => "NotAllowedError",
    }
}

fn credential(cx: &mut Cx<'_, '_, '_>, id: Vec<u8>, r: Resp, iface: Iface) -> Obj {
    let raw_id = crate::dom::blobs::array_buffer(cx, &id);
    let response = cx.st().alloc(iface, Slot::AuthResponse(Box::new(r)));
    Obj::Slab(cx.st().alloc(
        Iface::PublicKeyCredential,
        Slot::Credential(Box::new(Cred {
            id,
            raw_id,
            response,
        })),
    ))
}

/// `Credential`.
pub mod credential {
    use super::*;

    pub fn get_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(URL_SAFE_NO_PAD.encode(cred(cx, this)?.id))
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        cred(cx, this)?;
        Ok("public-key".into())
    }

    /// There is no autofill picker: sites show their passkey button instead.
    pub fn is_conditional_mediation_available(cx: &mut Cx<'_, '_, '_>) -> R<Value> {
        resolved(cx, false)
    }
}

/// `PublicKeyCredential`.
pub mod public_key_credential {
    use super::*;

    pub fn get_raw_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        Ok(cred(cx, this)?.raw_id)
    }

    pub fn get_response(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Slab(cred(cx, this)?.response))
    }

    pub fn get_authenticator_attachment(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        cred(cx, this)?;
        Ok(Some("platform".into()))
    }

    pub fn is_conditional_mediation_available(cx: &mut Cx<'_, '_, '_>) -> R<Value> {
        resolved(cx, false)
    }

    /// True when the session has a passkey store (asked of the environment).
    pub fn is_user_verifying_platform_authenticator_available(cx: &mut Cx<'_, '_, '_>) -> R<Value> {
        let (resolver, promise) = crate::dom::net::promise(cx)?;
        let realm = cx.realm.id;
        cx.realm
            .shared
            .borrow_mut()
            .passkey_calls
            .push_back(Waiting {
                realm,
                call: PasskeyCall::Available,
                resolver,
                client_data: Vec::new(),
            });
        Ok(promise)
    }
}

/// `AuthenticatorResponse`.
pub mod authenticator_response {
    use super::*;

    pub fn get_client_data_json(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        Ok(resp(cx, this)?.client_data)
    }
}

/// `AuthenticatorAttestationResponse`.
pub mod authenticator_attestation_response {
    use super::*;

    fn att(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(Resp, Value, Vec<u8>)> {
        let r = resp(cx, this)?;
        let (o, k) = r.attestation.clone().ok_or_else(illegal)?;
        Ok((r, o, k))
    }

    pub fn get_attestation_object(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        Ok(att(cx, this)?.1)
    }

    pub fn get_transports(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<String>> {
        att(cx, this)?;
        Ok(vec!["internal".into()])
    }

    pub fn get_authenticator_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let (r, _, _) = att(cx, this)?;
        Ok(crate::dom::blobs::array_buffer(cx, &r.authenticator_data))
    }

    pub fn get_public_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let (_, _, k) = att(cx, this)?;
        Ok(crate::dom::blobs::array_buffer(cx, &k))
    }

    pub fn get_public_key_algorithm(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        att(cx, this)?;
        Ok(-7)
    }
}

/// `AuthenticatorAssertionResponse`.
pub mod authenticator_assertion_response {
    use super::*;

    fn asr(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(Value, Value, Option<Value>)> {
        resp(cx, this)?.assertion.ok_or_else(illegal)
    }

    pub fn get_authenticator_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        Ok(asr(cx, this)?.0)
    }

    pub fn get_signature(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        Ok(asr(cx, this)?.1)
    }

    pub fn get_user_handle(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match asr(cx, this)?.2 {
            Some(v) => Ok(v),
            None => Ok(null(cx)),
        }
    }
}

/// `navigator.credentials`.
pub(crate) fn container(cx: &mut Cx<'_, '_, '_>) -> Obj {
    crate::dom::window_same(
        cx,
        "credentials",
        Iface::CredentialsContainer,
        Slot::Clipboard,
    )
}
