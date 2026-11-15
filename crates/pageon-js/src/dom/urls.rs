//! `URL`, `URLSearchParams` and `FormData` (string entries).

use ::url::Url;
use pageon_dom::{Edge, NodeId};

use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

/// `application/x-www-form-urlencoded` serialization.
/// Entries as `application/x-www-form-urlencoded` (files by name).
pub fn serialize_entries(pairs: &[(String, crate::dom::blobs::Entry)]) -> String {
    let text: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.clone(), v.text())).collect();
    serialize(&text)
}

pub fn serialize(pairs: &[(String, String)]) -> String {
    ::url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    ::url::form_urlencoded::parse(q.strip_prefix('?').unwrap_or(q).as_bytes())
        .into_owned()
        .collect()
}

fn parse(url: &str, base: Option<&str>) -> R<Url> {
    let base = match base {
        Some(b) => Some(Url::parse(b).map_err(|_| Error::Type(format!("Invalid base URL {b:?}")))?),
        None => None,
    };
    Url::options()
        .base_url(base.as_ref())
        .parse(url)
        .map_err(|_| Error::Type(format!("Invalid URL {url:?}")))
}

/// `URL`.
pub mod url {
    use super::*;

    /// `URL.createObjectURL(blob)`: a `blob:` URL a worker can start from.
    pub fn create_object_url(cx: &mut Cx<'_, '_, '_>, obj: Value) -> R<String> {
        crate::dom::workers::create_object_url(cx, &obj)
    }

    /// `URL.revokeObjectURL(url)`.
    pub fn revoke_object_url(cx: &mut Cx<'_, '_, '_>, url: String) -> R<()> {
        cx.realm.shared.borrow_mut().blob_urls.remove(&url);
        Ok(())
    }

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&Url) -> T) -> R<T> {
        match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Url(u, _)) => Ok(f(u)),
            _ => Err(illegal()),
        }
    }

    /// Changes the URL, then refreshes its `searchParams`.
    fn set(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut Url)) -> R<()> {
        let id = this.slab()?;
        let mut st = cx.realm.st.borrow_mut();
        let (query, params) = match st.slot_mut(id) {
            Some(Slot::Url(u, p)) => {
                f(u);
                (u.query().unwrap_or_default().to_string(), *p)
            }
            _ => return Err(illegal()),
        };
        if let Some(p) = params
            && let Some(Slot::Params(list, _)) = st.slot_mut(p)
        {
            *list = parse_query(&query);
        }
        Ok(())
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, url: String, base: Option<String>) -> R<Obj> {
        let u = super::parse(&url, base.as_deref())?;
        Ok(cx.alloc(Iface::URL, Slot::Url(Box::new(u), None)))
    }

    pub fn parse(cx: &mut Cx<'_, '_, '_>, url: String, base: Option<String>) -> R<Option<Obj>> {
        Ok(super::parse(&url, base.as_deref())
            .ok()
            .map(|u| cx.alloc(Iface::URL, Slot::Url(Box::new(u), None))))
    }

    pub fn can_parse(_cx: &mut Cx<'_, '_, '_>, url: String, base: Option<String>) -> R<bool> {
        Ok(super::parse(&url, base.as_deref()).is_ok())
    }

    pub fn get_href(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::href(u).to_string())
    }

    pub fn set_href(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let parsed = Url::parse(&v).map_err(|_| Error::Type(format!("Invalid URL {v:?}")))?;
        set(cx, this, |u| *u = parsed)
    }

    pub fn to_json(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get_href(cx, this)
    }

    pub fn get_origin(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, ::url::quirks::origin)
    }

    pub fn get_protocol(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::protocol(u).to_string())
    }

    pub fn set_protocol(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_protocol(u, &v);
        })
    }

    pub fn get_username(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::username(u).to_string())
    }

    pub fn set_username(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_username(u, &v);
        })
    }

    pub fn get_password(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::password(u).to_string())
    }

    pub fn set_password(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_password(u, &v);
        })
    }

    pub fn get_host(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::host(u).to_string())
    }

    pub fn set_host(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_host(u, &v);
        })
    }

    pub fn get_hostname(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::hostname(u).to_string())
    }

    pub fn set_hostname(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_hostname(u, &v);
        })
    }

    pub fn get_port(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::port(u).to_string())
    }

    pub fn set_port(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| {
            let _ = ::url::quirks::set_port(u, &v);
        })
    }

    pub fn get_pathname(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::pathname(u).to_string())
    }

    pub fn set_pathname(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| ::url::quirks::set_pathname(u, &v))
    }

    pub fn get_search(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::search(u).to_string())
    }

    pub fn set_search(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| ::url::quirks::set_search(u, &v))
    }

    pub fn get_hash(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |u| ::url::quirks::hash(u).to_string())
    }

    pub fn set_hash(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set(cx, this, |u| ::url::quirks::set_hash(u, &v))
    }

    pub fn get_search_params(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.slab()?;
        let (existing, query) = match cx.realm.st.borrow().slot(id) {
            Some(Slot::Url(u, p)) => (*p, u.query().unwrap_or_default().to_string()),
            _ => return Err(illegal()),
        };
        if let Some(p) = existing {
            return Ok(Obj::Slab(p));
        }
        let Obj::Slab(p) = cx.alloc(
            Iface::URLSearchParams,
            Slot::Params(parse_query(&query), Some(id)),
        ) else {
            return Err(illegal());
        };
        if let Some(Slot::Url(_, link)) = cx.realm.st.borrow_mut().slot_mut(id) {
            *link = Some(p);
        }
        Ok(Obj::Slab(p))
    }
}

/// `URLSearchParams`.
pub mod url_search_params {
    use super::*;

    fn with<T>(
        cx: &Cx<'_, '_, '_>,
        this: Obj,
        f: impl FnOnce(&mut Vec<(String, String)>) -> T,
    ) -> R<T> {
        let id = this.slab()?;
        let mut st = cx.realm.st.borrow_mut();
        let (out, owner, query) = match st.slot_mut(id) {
            Some(Slot::Params(list, owner)) => {
                let out = f(list);
                (out, *owner, serialize(list))
            }
            _ => return Err(illegal()),
        };
        if let Some(u) = owner
            && let Some(Slot::Url(url, _)) = st.slot_mut(u)
        {
            url.set_query((!query.is_empty()).then_some(query.as_str()));
        }
        Ok(out)
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, init: Value) -> R<Obj> {
        let v = cx.local(&init);
        let list = if let Some((Obj::Slab(id), Iface::URLSearchParams)) = cv::unwrap_any(cx, v) {
            match cx.realm.st.borrow().slot(id) {
                Some(Slot::Params(p, _)) => p.clone(),
                _ => Vec::new(),
            }
        } else if v.is_object() && !v.is_string_object() {
            let obj = v8::Local::<v8::Object>::try_from(v).map_err(|_| illegal())?;
            let sym = v8::Symbol::get_iterator(cx.scope);
            let iterable = obj
                .get(cx.scope, sym.into())
                .is_some_and(|f| f.is_function());
            if iterable {
                let pairs = cv::seq(cx, v, |cx, p| cv::seq(cx, p, cv::string))?;
                pairs
                    .into_iter()
                    .map(|p| {
                        <[String; 2]>::try_from(p)
                            .map(|[k, v]| (k, v))
                            .map_err(|_| Error::Type("Each query pair must have two items".into()))
                    })
                    .collect::<R<_>>()?
            } else {
                let keys = obj
                    .get_own_property_names(cx.scope, v8::GetPropertyNamesArgs::default())
                    .ok_or(Error::Terminated)?;
                let mut out = Vec::new();
                for i in 0..keys.length() {
                    let k = keys.get_index(cx.scope, i).ok_or(Error::Terminated)?;
                    let val = obj.get(cx.scope, k).ok_or(Error::Terminated)?;
                    out.push((cv::string(cx, k)?, cv::string(cx, val)?));
                }
                out
            }
        } else {
            parse_query(&cv::string(cx, v)?)
        };
        Ok(cx.alloc(Iface::URLSearchParams, Slot::Params(list, None)))
    }

    pub fn get_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        with(cx, this, |l| u32::try_from(l.len()).unwrap_or(u32::MAX))
    }

    pub fn append(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        with(cx, this, |l| l.push((name, value)))
    }

    pub fn delete(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        value: Option<String>,
    ) -> R<()> {
        with(cx, this, |l| {
            l.retain(|(k, v)| !(*k == name && value.as_ref().is_none_or(|x| x == v)));
        })
    }

    pub fn get(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<String>> {
        with(cx, this, |l| {
            l.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone())
        })
    }

    pub fn get_all(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Vec<String>> {
        with(cx, this, |l| {
            l.iter()
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
                .collect()
        })
    }

    pub fn has(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: Option<String>) -> R<bool> {
        with(cx, this, |l| {
            l.iter()
                .any(|(k, v)| *k == name && value.as_ref().is_none_or(|x| x == v))
        })
    }

    pub fn set(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        with(cx, this, |l| match l.iter().position(|(k, _)| *k == name) {
            Some(i) => {
                l[i].1 = value;
                let mut j = i + 1;
                while j < l.len() {
                    if l[j].0 == name {
                        l.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => l.push((name, value)),
        })
    }

    pub fn sort(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |l| {
            l.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        })
    }

    pub fn to_string(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |l| serialize(l))
    }

    pub fn pairs(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<(String, String)>> {
        with(cx, this, |l| l.clone())
    }
}

/// The entries a form submits: named, enabled controls (checkboxes and radios only when
/// checked; no file inputs or buttons unless the button is the submitter).
pub(crate) fn form_entries(
    cx: &mut Cx<'_, '_, '_>,
    form: NodeId,
    submitter: Option<NodeId>,
) -> R<Vec<(String, crate::dom::blobs::Entry)>> {
    let controls: Vec<NodeId> = pageon_dom::forms::listed(&cx.dom(), form);
    let mut out = Vec::new();
    for id in controls {
        let (tag, name, ty, disabled, face) = {
            let dom = cx.dom();
            let Some(n) = dom.get(id) else { continue };
            (
                n.local_name().unwrap_or_default().to_string(),
                n.attr("name").unwrap_or_default().to_string(),
                n.attr("type").unwrap_or_default().to_ascii_lowercase(),
                pageon_dom::forms::disabled(&dom, id),
                pageon_dom::forms::is_form_associated(&dom, id),
            )
        };
        if disabled {
            continue;
        }
        // A form-associated custom element: the entries `setFormValue` gave.
        if face {
            let value = cx
                .dom()
                .internals(id)
                .map(|i| i.value.clone())
                .unwrap_or_default();
            for (k, v) in value {
                match k {
                    Some(k) => out.push((k, crate::dom::blobs::Entry::Text(v))),
                    None if !name.is_empty() => {
                        out.push((name.clone(), crate::dom::blobs::Entry::Text(v)));
                    }
                    None => {}
                }
            }
            continue;
        }
        if name.is_empty() {
            continue;
        }
        let obj = Obj::Node(id);
        match tag.as_str() {
            "input" => match ty.as_str() {
                "checkbox" | "radio" => {
                    if crate::dom::html_input_element::get_checked(cx, obj)? {
                        let v = crate::dom::html_input_element::get_value(cx, obj)?;
                        out.push((name, crate::dom::blobs::Entry::Text(v)));
                    }
                }
                // A file input's files; with none, an empty file part (as Chrome sends).
                "file" => {
                    let files = cx.st().files.get(&id).cloned().unwrap_or_default();
                    if files.is_empty() {
                        out.push((
                            name,
                            crate::dom::blobs::Entry::File(std::rc::Rc::new(
                                crate::dom::blobs::BlobData::file(
                                    String::new(),
                                    "application/octet-stream".into(),
                                    Vec::new(),
                                    0,
                                ),
                            )),
                        ));
                    } else {
                        for f in files {
                            out.push((name.clone(), crate::dom::blobs::Entry::File(f)));
                        }
                    }
                }
                "image" | "reset" | "button" => {}
                "submit" => {
                    if submitter == Some(id) {
                        out.push((
                            name,
                            crate::dom::blobs::Entry::Text(
                                crate::dom::html_input_element::get_value(cx, obj)?,
                            ),
                        ));
                    }
                }
                _ => out.push((
                    name,
                    crate::dom::blobs::Entry::Text(crate::dom::html_input_element::get_value(
                        cx, obj,
                    )?),
                )),
            },
            "textarea" => out.push((
                name,
                crate::dom::blobs::Entry::Text(crate::dom::html_text_area_element::get_value(
                    cx, obj,
                )?),
            )),
            "select" => {
                let opts: Vec<NodeId> = {
                    let dom = cx.dom();
                    dom.traverse(id)
                        .filter_map(|e| match e {
                            Edge::Enter(o) if dom.get(o).is_some_and(|n| n.is_html("option")) => {
                                Some(o)
                            }
                            _ => None,
                        })
                        .collect()
                };
                for o in opts {
                    if crate::dom::html_option_element::selected(cx, o) {
                        let v = crate::dom::html_option_element::get_value(cx, Obj::Node(o))?;
                        out.push((name.clone(), crate::dom::blobs::Entry::Text(v)));
                    }
                }
            }
            "button" if submitter == Some(id) => {
                let v = cx
                    .dom()
                    .get(id)
                    .and_then(|n| n.attr("value"))
                    .unwrap_or_default()
                    .to_string();
                out.push((name, crate::dom::blobs::Entry::Text(v)));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `FormData`: text and file entries in order.
pub mod form_data {
    use super::*;
    use crate::dom::blobs::{Entry, blob_of, wrap};

    fn with<T>(
        cx: &Cx<'_, '_, '_>,
        this: Obj,
        f: impl FnOnce(&mut Vec<(String, Entry)>) -> T,
    ) -> R<T> {
        match cx.realm.st.borrow_mut().slot_mut(this.slab()?) {
            Some(Slot::FormData(l)) => Ok(f(l)),
            _ => Err(illegal()),
        }
    }

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        form: Option<Obj>,
        submitter: Option<Obj>,
    ) -> R<Obj> {
        let list = match form {
            Some(f) => {
                let s = submitter.map(Obj::node).transpose()?;
                form_entries(cx, f.node()?, s)?
            }
            None => Vec::new(),
        };
        Ok(cx.alloc(Iface::FormData, Slot::FormData(list)))
    }

    /// A blob as a file entry: named `filename`, else its own name, else `blob`.
    fn file_entry(cx: &Cx<'_, '_, '_>, blob: Obj, filename: Option<String>) -> R<Entry> {
        let b = blob_of(cx, blob)?;
        let modified = b.file.as_ref().map_or(0, |(_, m)| *m);
        let name = filename
            .or_else(|| b.file.as_ref().map(|(n, _)| n.clone()))
            .unwrap_or_else(|| "blob".into());
        Ok(Entry::File(std::rc::Rc::new(crate::dom::blobs::BlobData {
            bytes: b.bytes.clone(),
            content_type: b.content_type.clone(),
            file: Some((name, modified)),
        })))
    }

    fn put(l: &mut Vec<(String, Entry)>, name: String, value: Entry) {
        match l.iter().position(|(k, _)| *k == name) {
            Some(i) => {
                l[i].1 = value;
                let mut j = i + 1;
                while j < l.len() {
                    if l[j].0 == name {
                        l.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => l.push((name, value)),
        }
    }

    pub fn append(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        with(cx, this, |l| l.push((name, Entry::Text(value))))
    }

    pub fn append_name_blob_value_filename(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        blob: Obj,
        filename: Option<String>,
    ) -> R<()> {
        let e = file_entry(cx, blob, filename)?;
        with(cx, this, |l| l.push((name, e)))
    }

    pub fn delete(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<()> {
        with(cx, this, |l| l.retain(|(k, _)| *k != name))
    }

    fn value(cx: &mut Cx<'_, '_, '_>, e: Entry) -> Value {
        match e {
            Entry::Text(s) => {
                let v = cx.str(&s).into();
                cx.keep(v)
            }
            Entry::File(f) => {
                let o = wrap(cx, f);
                let v: v8::Local<v8::Value> = cx.wrap(o).into();
                cx.keep(v)
            }
        }
    }

    pub fn get(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<Value>> {
        let e = with(cx, this, |l| {
            l.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone())
        })?;
        Ok(e.map(|e| value(cx, e)))
    }

    pub fn get_all(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Vec<Value>> {
        let es: Vec<Entry> = with(cx, this, |l| {
            l.iter()
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
                .collect()
        })?;
        Ok(es.into_iter().map(|e| value(cx, e)).collect())
    }

    pub fn has(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<bool> {
        with(cx, this, |l| l.iter().any(|(k, _)| *k == name))
    }

    pub fn set(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        with(cx, this, |l| put(l, name, Entry::Text(value)))
    }

    pub fn set_name_blob_value_filename(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        blob: Obj,
        filename: Option<String>,
    ) -> R<()> {
        let e = file_entry(cx, blob, filename)?;
        with(cx, this, |l| put(l, name, e))
    }

    /// Iteration: files appear by name (a deviation: Chrome yields the `File`).
    pub fn pairs(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<(String, String)>> {
        with(cx, this, |l| {
            l.iter().map(|(k, v)| (k.clone(), v.text())).collect()
        })
    }
}
