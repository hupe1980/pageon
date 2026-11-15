//! `DataTransfer`, `DataTransferItemList` and `DataTransferItem`: the data of a drag (and the
//! file lists pages build, as Playwright's `setInputFiles` does).
//!
//! The data is readable in `dragstart` (read/write) and `drop` (read-only); during the other
//! drag events it is protected: the types show, the values do not.

use std::rc::Rc;

use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::dom::blobs::BlobData;
use crate::generated::Iface;
use crate::realm::{Slot, Timer};

/// One item of a drag's data.
#[derive(Debug, Clone)]
pub enum Item {
    /// A string of a type (`text/plain`).
    Text {
        /// The type, lower-cased.
        ty: String,
        /// The data.
        data: String,
    },
    /// A file.
    File(Rc<BlobData>),
}

/// What scripts may do with the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `dragstart` and a page's own `new DataTransfer()`.
    ReadWrite,
    /// `drop`.
    ReadOnly,
    /// The other drag events: types only.
    Protected,
}

/// A `DataTransfer`.
#[derive(Debug, Clone)]
pub struct DtData {
    /// The items.
    pub items: Vec<Item>,
    /// `dropEffect`.
    pub drop_effect: String,
    /// `effectAllowed`.
    pub effect_allowed: String,
    /// The mode.
    pub mode: Mode,
    /// The `items` object, once created (`[SameObject]`).
    list: Option<u32>,
}

impl DtData {
    /// The data of a drag the engine starts.
    pub fn drag(items: Vec<Item>) -> Self {
        Self {
            items,
            drop_effect: "none".into(),
            effect_allowed: "uninitialized".into(),
            mode: Mode::ReadWrite,
            list: None,
        }
    }

    /// The string items, as (type, data) pairs.
    pub fn strings(&self) -> Vec<(String, String)> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { ty, data } => Some((ty.clone(), data.clone())),
                Item::File(_) => None,
            })
            .collect()
    }
}

fn normalize(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "text" => "text/plain".into(),
        "url" => "text/uri-list".into(),
        f => f.into(),
    }
}

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

fn with<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut DtData) -> T) -> R<T> {
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::DataTransfer(d)) => Ok(f(d)),
        _ => Err(illegal()),
    }
}

/// The data transfer behind a `DataTransfer`, its item list or one of its items.
fn owner(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(u32, Option<usize>)> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::DataTransfer(_)) => Ok((id, None)),
        Some(Slot::DtItems(dt)) => Ok((*dt, None)),
        Some(Slot::DtItem(dt, i)) => Ok((*dt, Some(*i))),
        _ => Err(illegal()),
    }
}

/// A new `DataTransfer` holding `data` (the engine's drags).
pub(crate) fn new_data_transfer(cx: &mut Cx<'_, '_, '_>, data: DtData) -> u32 {
    cx.st()
        .alloc(Iface::DataTransfer, Slot::DataTransfer(Box::new(data)))
}

/// Sets the mode of a data transfer (the engine, between drag events).
pub(crate) fn set_mode(cx: &Cx<'_, '_, '_>, id: u32, mode: Mode) {
    let _ = with(cx, id, |d| d.mode = mode);
}

fn item_obj(cx: &mut Cx<'_, '_, '_>, dt: u32, index: usize) -> Obj {
    cx.alloc(Iface::DataTransferItem, Slot::DtItem(dt, index))
}

fn file_obj(cx: &mut Cx<'_, '_, '_>, b: Rc<BlobData>) -> Obj {
    cx.alloc(Iface::File, Slot::Blob(b))
}

/// `DataTransfer`.
#[allow(missing_docs)]
pub mod data_transfer {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        let mut d = DtData::drag(Vec::new());
        d.effect_allowed = "none".into();
        Ok(cx.alloc(Iface::DataTransfer, Slot::DataTransfer(Box::new(d))))
    }

    pub fn get_drop_effect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this.slab()?, |d| d.drop_effect.clone())
    }

    pub fn set_drop_effect(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        with(cx, this.slab()?, |d| {
            if matches!(v.as_str(), "none" | "copy" | "link" | "move") {
                d.drop_effect = v;
            }
        })
    }

    pub fn get_effect_allowed(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this.slab()?, |d| d.effect_allowed.clone())
    }

    pub fn set_effect_allowed(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        with(cx, this.slab()?, |d| {
            let valid = matches!(
                v.as_str(),
                "none"
                    | "copy"
                    | "copyLink"
                    | "copyMove"
                    | "link"
                    | "linkMove"
                    | "move"
                    | "all"
                    | "uninitialized"
            );
            if valid && d.mode == Mode::ReadWrite {
                d.effect_allowed = v;
            }
        })
    }

    pub fn get_items(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.slab()?;
        if let Some(l) = with(cx, id, |d| d.list)? {
            return Ok(Obj::Slab(l));
        }
        let o = cx.alloc(Iface::DataTransferItemList, Slot::DtItems(id));
        let l = o.slab()?;
        with(cx, id, |d| d.list = Some(l))?;
        Ok(o)
    }

    pub fn set_drag_image(
        _cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        _image: Obj,
        _x: i32,
        _y: i32,
    ) -> R<()> {
        Ok(())
    }

    pub fn get_types(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<String>> {
        with(cx, this.slab()?, |d| {
            let mut out: Vec<String> = Vec::new();
            for i in &d.items {
                if let Item::Text { ty, .. } = i
                    && !out.contains(ty)
                {
                    out.push(ty.clone());
                }
            }
            if d.items.iter().any(|i| matches!(i, Item::File(_))) {
                out.push("Files".into());
            }
            out
        })
    }

    pub fn get_data(cx: &mut Cx<'_, '_, '_>, this: Obj, format: String) -> R<String> {
        let f = normalize(&format);
        with(cx, this.slab()?, |d| {
            if d.mode == Mode::Protected {
                return String::new();
            }
            let found = d.items.iter().find_map(|i| match i {
                Item::Text { ty, data } if *ty == f => Some(data.clone()),
                _ => None,
            });
            let data = found.unwrap_or_default();
            if format.eq_ignore_ascii_case("url") {
                // The first URL of the list.
                return data
                    .lines()
                    .find(|l| !l.starts_with('#') && !l.trim().is_empty())
                    .unwrap_or_default()
                    .to_string();
            }
            data
        })
    }

    pub fn set_data(cx: &mut Cx<'_, '_, '_>, this: Obj, format: String, data: String) -> R<()> {
        let f = normalize(&format);
        with(cx, this.slab()?, |d| {
            if d.mode != Mode::ReadWrite {
                return;
            }
            d.items
                .retain(|i| !matches!(i, Item::Text { ty, .. } if *ty == f));
            d.items.push(Item::Text { ty: f, data });
        })
    }

    pub fn clear_data(cx: &mut Cx<'_, '_, '_>, this: Obj, format: Option<String>) -> R<()> {
        let f = format.as_deref().map(normalize);
        with(cx, this.slab()?, |d| {
            if d.mode != Mode::ReadWrite {
                return;
            }
            d.items.retain(|i| match i {
                Item::Text { ty, .. } => f.as_ref().is_some_and(|f| f != ty),
                Item::File(_) => true,
            });
        })
    }

    pub fn get_files(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let files = with(cx, this.slab()?, |d| {
            if d.mode == Mode::Protected {
                return Vec::new();
            }
            d.items
                .iter()
                .filter_map(|i| match i {
                    Item::File(b) => Some(b.clone()),
                    Item::Text { .. } => None,
                })
                .collect()
        })?;
        Ok(cx.alloc(Iface::FileList, Slot::FileList(files)))
    }
}

/// `DataTransferItemList`.
#[allow(missing_docs)]
pub mod data_transfer_item_list {
    use super::*;

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        let (dt, _) = owner(cx, this)?;
        with(cx, dt, |d| d.items.len() as u32)
    }

    /// `add(data, type)`.
    pub fn add(cx: &mut Cx<'_, '_, '_>, this: Obj, data: String, ty: String) -> R<Option<Obj>> {
        let (dt, _) = owner(cx, this)?;
        let ty = ty.to_ascii_lowercase();
        let index = with(cx, dt, |d| {
            if d.mode != Mode::ReadWrite {
                return Ok(None);
            }
            if d.items
                .iter()
                .any(|i| matches!(i, Item::Text { ty: t, .. } if *t == ty))
            {
                return Err(Error::Dom(
                    "NotSupportedError",
                    format!(
                        "Failed to execute 'add' on 'DataTransferItemList': An item already exists for type '{ty}'."
                    ),
                ));
            }
            d.items.push(Item::Text { ty, data });
            Ok(Some(d.items.len() - 1))
        })??;
        Ok(index.map(|i| item_obj(cx, dt, i)))
    }

    /// `add(file)`.
    pub fn add_file(cx: &mut Cx<'_, '_, '_>, this: Obj, file: Rc<BlobData>) -> R<Option<Obj>> {
        let (dt, _) = owner(cx, this)?;
        let index = with(cx, dt, |d| {
            (d.mode == Mode::ReadWrite).then(|| {
                d.items.push(Item::File(file));
                d.items.len() - 1
            })
        })?;
        Ok(index.map(|i| item_obj(cx, dt, i)))
    }

    pub fn remove(cx: &mut Cx<'_, '_, '_>, this: Obj, index: u32) -> R<()> {
        let (dt, _) = owner(cx, this)?;
        with(cx, dt, |d| {
            if d.mode != Mode::ReadWrite {
                return Err(Error::Dom(
                    "InvalidStateError",
                    "Failed to execute 'remove' on 'DataTransferItemList': The list is not writable.".into(),
                ));
            }
            if (index as usize) < d.items.len() {
                d.items.remove(index as usize);
            }
            Ok(())
        })?
    }

    pub fn clear(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let (dt, _) = owner(cx, this)?;
        with(cx, dt, |d| {
            if d.mode == Mode::ReadWrite {
                d.items.clear();
            }
        })
    }

    /// `list[i]`.
    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, index: u32) -> R<Option<Obj>> {
        let (dt, _) = owner(cx, this)?;
        let ok = with(cx, dt, |d| (index as usize) < d.items.len())?;
        Ok(ok.then(|| item_obj(cx, dt, index as usize)))
    }

    /// `add` with either overload (string and type, or a `File`).
    pub(crate) fn add_any(
        scope: &mut v8::PinScope,
        args: v8::FunctionCallbackArguments,
        mut rv: v8::ReturnValue,
    ) {
        cv::call(scope, &args, &mut rv, |cx, args| {
            let this = cx.this(args, Iface::DataTransferItemList)?;
            let a0 = args.get(0);
            if args.length() == 1 {
                let file = match cv::unwrap_any(cx, a0) {
                    Some((Obj::Slab(id), Iface::File)) => match cx.st().slot(id) {
                        Some(Slot::Blob(b)) => Some(b.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(file) = file else {
                    return Err(Error::Type(
                        "Failed to execute 'add' on 'DataTransferItemList': parameter 1 is not of type 'File'.".into(),
                    ));
                };
                let r = add_file(cx, this, file)?;
                return cx.ret(r);
            }
            if args.length() == 0 {
                return Err(Error::args("DataTransferItemList.add", 0));
            }
            let data = cv::string(cx, a0)?;
            let ty = cv::string(cx, args.get(1))?;
            let r = add(cx, this, data, ty)?;
            cx.ret(r)
        });
    }
}

/// `DataTransferItem`.
#[allow(missing_docs)]
pub mod data_transfer_item {
    use super::*;

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&Item, Mode) -> T) -> R<Option<T>> {
        let (dt, i) = owner(cx, this)?;
        let Some(i) = i else { return Err(illegal()) };
        with(cx, dt, |d| d.items.get(i).map(|item| f(item, d.mode)))
    }

    pub fn get_kind(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(get(cx, this, |i, _| match i {
            Item::Text { .. } => "string",
            Item::File(_) => "file",
        })?
        .unwrap_or_default()
        .to_string())
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(get(cx, this, |i, _| match i {
            Item::Text { ty, .. } => ty.clone(),
            Item::File(b) => b.content_type.clone(),
        })?
        .unwrap_or_default())
    }

    pub fn get_as_string(cx: &mut Cx<'_, '_, '_>, this: Obj, callback: Option<Value>) -> R<()> {
        let Some(cb) = callback else { return Ok(()) };
        let data = get(cx, this, |i, mode| match (i, mode) {
            (Item::Text { data, .. }, Mode::ReadWrite | Mode::ReadOnly) => Some(data.clone()),
            _ => None,
        })?
        .flatten();
        let Some(data) = data else { return Ok(()) };
        // The callback runs in a task of its own.
        let s: v8::Local<v8::Value> = v8::String::new(cx.scope, &data)
            .ok_or(Error::Terminated)?
            .into();
        let arg = cx.keep(s);
        let realm = cx.realm.id;
        cx.realm.shared.borrow_mut().schedule(
            Timer {
                id: 0,
                realm,
                cb,
                args: vec![arg],
                interval: None,
                frame: false,
                idle: false,
                caused: false,
            },
            0.0,
        );
        Ok(())
    }

    pub fn get_as_file(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let file = get(cx, this, |i, mode| match (i, mode) {
            (Item::File(b), Mode::ReadWrite | Mode::ReadOnly) => Some(b.clone()),
            _ => None,
        })?
        .flatten();
        Ok(file.map(|b| file_obj(cx, b)))
    }
}
