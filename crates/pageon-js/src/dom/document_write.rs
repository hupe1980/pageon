//! `document.write` and `writeln` while the page parses: the text goes right after the
//! running script and is parsed next (scripts in it run like any parsed script). Once
//! parsing has ended they throw: pageon does not replace a loaded document.

use crate::cv::{Error, R};

fn text(cx: &mut crate::cv::Cx<'_, '_, '_>, args: &v8::FunctionCallbackArguments) -> R<String> {
    let mut out = String::new();
    for i in 0..args.length() {
        out.push_str(&crate::cv::string(cx, args.get(i))?);
    }
    Ok(out)
}

fn append(cx: &mut crate::cv::Cx<'_, '_, '_>, s: &str) -> R<()> {
    let mut st = cx.st();
    match st.written.as_mut() {
        Some(w) => {
            w.push_str(s);
            Ok(())
        }
        None => Err(Error::dom(
            "NotSupportedError",
            "document.write after the page has loaded is not supported (it would replace the document)",
        )),
    }
}

pub(crate) fn write(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        cx.this(args, crate::generated::Iface::Document)?;
        let t = text(cx, args)?;
        append(cx, &t)?;
        cx.ret(())
    });
}

pub(crate) fn writeln(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        cx.this(args, crate::generated::Iface::Document)?;
        let t = text(cx, args)?;
        append(cx, &format!("{t}\n"))?;
        cx.ret(())
    });
}
