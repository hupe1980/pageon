//! IndexedDB: fake-indexeddb 6.2.8 (Apache-2.0; tested against the IndexedDB
//! web-platform-tests), bundled with pageon's persistence (`indexeddb/entry.js`) and pinned by
//! SHA-256. A document evaluates it the first time it touches an IndexedDB global; databases
//! are kept per origin by the session, as `localStorage` is.
//!
//! Regenerate: in a directory with `npm i fake-indexeddb@6.2.8 esbuild@0.28.2`, copy
//! `entry.js` there and run `npx esbuild entry.js --bundle --minify --format=iife
//! --target=es2022 --outfile=fake-indexeddb.js`, then prepend the licence line.

use crate::cv::{Cx, Error, Out};

/// The vendored bundle.
pub(crate) const SOURCE: &str = include_str!("indexeddb/fake-indexeddb.js");

/// Serialized databases an origin may keep.
pub(crate) const MAX_BYTES: usize = 50 << 20;

fn origin(cx: &Cx<'_, '_, '_>) -> Option<String> {
    let o = cx.st().origin.clone();
    (o != "null" && !o.is_empty()).then_some(o)
}

/// `load()`: the origin's database snapshots.
fn host_load(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, _| {
        let snaps: Vec<Vec<u8>> = match origin(cx) {
            Some(o) => {
                let net = cx.st().net.clone();
                let mut net = net.borrow_mut();
                net.idb(&o).values().cloned().collect()
            }
            None => Vec::new(),
        };
        let mut out = Vec::new();
        for bytes in snaps {
            // A snapshot this build cannot read is skipped (the entry reports it).
            if let Ok(v) = crate::dom::session::deserialize(cx, &bytes) {
                out.push(Out::Value(v));
            }
        }
        Ok(Out::List(out))
    });
}

/// `save(name, snapshot | null)`: keeps (or removes) a database; `false` over the limit.
fn host_save(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let name = crate::cv::string(cx, args.get(0))?;
        let Some(o) = origin(cx) else {
            return Ok(Out::Bool(true));
        };
        let snap = args.get(1);
        let net = cx.st().net.clone();
        if snap.is_null_or_undefined() {
            net.borrow_mut().idb(&o).remove(&name);
            return Ok(Out::Bool(true));
        }
        let v = cx.keep(snap);
        let bytes = crate::dom::session::serialize(cx, &v)?;
        let mut net = net.borrow_mut();
        let dbs = net.idb(&o);
        let others: usize = dbs
            .iter()
            .filter(|(k, _)| **k != name)
            .map(|(_, b)| b.len())
            .sum();
        if others + bytes.len() > MAX_BYTES {
            return Ok(Out::Bool(false));
        }
        dbs.insert(name, bytes);
        Ok(Out::Bool(true))
    });
}

/// `host.idb()`: evaluates the bundle in the current context and returns its globals.
pub(crate) fn load(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, _| {
        let scope = &mut *cx.scope;
        let context = scope.get_current_context();
        let global = context.global(scope);
        let (Some(src), Some(name)) = (
            v8::String::new(scope, SOURCE),
            v8::String::new(scope, "__pageonIndexedDB"),
        ) else {
            return Err(Error::Terminated);
        };
        let host = v8::Object::new(scope);
        if let (Some(k), Some(f)) = (
            v8::String::new(scope, "load"),
            v8::Function::new(scope, host_load),
        ) {
            host.set(scope, k.into(), f.into());
        }
        if let (Some(k), Some(f)) = (
            v8::String::new(scope, "save"),
            v8::Function::new(scope, host_save),
        ) {
            host.set(scope, k.into(), f.into());
        }
        v8::tc_scope!(let tc, scope);
        if v8::Script::compile(tc, src, None)
            .and_then(|s| s.run(tc))
            .is_none()
        {
            return Err(Error::Type("IndexedDB failed to load".into()));
        }
        let Some(init) = global
            .get(tc, name.into())
            .and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())
        else {
            return Err(Error::Type("IndexedDB failed to load".into()));
        };
        global.delete(tc, name.into());
        let undefined = v8::undefined(tc).into();
        let Some(api) = init.call(tc, undefined, &[host.into()]) else {
            return Err(Error::Type("IndexedDB failed to start".into()));
        };
        let api = v8::Global::new(tc, api);
        Ok(Out::Value(api))
    });
}

#[cfg(test)]
mod tests {
    /// The vendored file's SHA-256 (update it, and THIRD_PARTY.md, deliberately).
    const SHA256: &str = "6507391575c70407c406e5c6ff28acfc722129a1ce3b7e305a94dfd71c92d344";

    #[test]
    fn the_vendored_source_is_the_pinned_one() {
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, super::SOURCE.as_bytes());
        let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, SHA256, "the vendored IndexedDB bundle changed");
    }
}
