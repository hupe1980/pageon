//! Web Storage: `localStorage` and `sessionStorage`, kept by the session (`pageon-net`) per
//! origin across loads.

use crate::cv::{Cx, Error, Obj, R};
use crate::generated::Iface;
use crate::realm::Slot;

/// Runs `f` on the storage area behind a `Storage` object.
pub(crate) fn area<T>(
    cx: &Cx<'_, '_, '_>,
    this: Obj,
    f: impl FnOnce(&mut pageon_net::Storage) -> T,
) -> R<T> {
    let local = match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::Storage(l)) => *l,
        _ => return Err(Error::Type("Illegal invocation".into())),
    };
    let (net, origin) = {
        let st = cx.realm.st.borrow();
        (st.net.clone(), st.origin.clone())
    };
    let mut net = net.borrow_mut();
    let area = if local {
        net.local_storage(&origin)
    } else {
        net.session_storage(&origin)
    };
    Ok(f(area))
}

fn quota() -> Error {
    Error::dom(
        "QuotaExceededError",
        "The quota has been exceeded (5 MiB per origin).",
    )
}

/// Sets an item (shared with the named-property interceptor).
pub(crate) fn set(cx: &Cx<'_, '_, '_>, this: Obj, key: &str, value: &str) -> R<()> {
    area(cx, this, |a| a.set(key, value))?.map_err(|_| quota())
}

/// `Storage`.
pub mod storage {
    use super::*;

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        area(cx, this, |a| u32::try_from(a.len()).unwrap_or(u32::MAX))
    }

    pub fn key(cx: &mut Cx<'_, '_, '_>, this: Obj, n: u32) -> R<Option<String>> {
        area(cx, this, |a| a.key(n as usize).map(str::to_string))
    }

    pub fn get_item(cx: &mut Cx<'_, '_, '_>, this: Obj, key: String) -> R<Option<String>> {
        area(cx, this, |a| a.get(&key).map(str::to_string))
    }

    pub fn set_item(cx: &mut Cx<'_, '_, '_>, this: Obj, key: String, value: String) -> R<()> {
        set(cx, this, &key, &value)
    }

    pub fn remove_item(cx: &mut Cx<'_, '_, '_>, this: Obj, key: String) -> R<()> {
        area(cx, this, |a| a.remove(&key))
    }

    pub fn clear(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        area(cx, this, pageon_net::Storage::clear)
    }
}

fn storage_object(cx: &mut Cx<'_, '_, '_>, key: &'static str, local: bool) -> Obj {
    if let Some(&id) = cx.st().window_same.get(key) {
        return Obj::Slab(id);
    }
    let o = cx.alloc(Iface::Storage, Slot::Storage(local));
    if let Obj::Slab(id) = o {
        cx.st().window_same.insert(key, id);
    }
    o
}

/// `WindowLocalStorage`.
pub mod window_local_storage {
    use super::*;

    pub fn get_local_storage(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(storage_object(cx, "localStorage", true))
    }
}

/// `WindowSessionStorage`.
pub mod window_session_storage {
    use super::*;

    pub fn get_session_storage(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(storage_object(cx, "sessionStorage", false))
    }
}
