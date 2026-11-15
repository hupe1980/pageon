//! The WHATWG Streams standard, from web-streams-polyfill 4.5.1 (MIT; spec-conformant, as
//! Deno's and Node's streams are JavaScript too), vendored and pinned by SHA-256. A document
//! evaluates it the first time it touches a stream global: documents that never do pay nothing.

/// The vendored source (`ponyfill.js`, unmodified).
pub(crate) const SOURCE: &str = include_str!("streams/web-streams-polyfill.js");

/// `host.streams()`: evaluates the source in the current context and returns its exports.
pub(crate) fn load(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let (Some(src), Some(name)) = (
        v8::String::new(scope, SOURCE),
        v8::String::new(scope, "WebStreamsPolyfill"),
    ) else {
        return;
    };
    v8::tc_scope!(let tc, scope);
    if v8::Script::compile(tc, src, None)
        .and_then(|s| s.run(tc))
        .is_none()
    {
        return;
    }
    if let Some(m) = global.get(tc, name.into()) {
        global.delete(tc, name.into());
        rv.set(m);
    }
}

#[cfg(test)]
mod tests {
    /// The vendored file's SHA-256 (update it, and THIRD_PARTY.md, deliberately).
    const SHA256: &str = "67e78881641f818767593f6e09085333a7006b61a7d5a5cb3af7c324822be1df";

    #[test]
    fn the_vendored_source_is_the_pinned_one() {
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, super::SOURCE.as_bytes());
        let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, SHA256, "the vendored streams source changed");
    }
}
