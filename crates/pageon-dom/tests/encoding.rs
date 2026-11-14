//! Encoding sniffing fixtures: BOM, HTTP charset, meta prescan, http-equiv, default, late meta.

use pageon_dom::encoding::{Source, decode};

fn check(bytes: &[u8], http: Option<&str>, want_text: &str, name: &str, source: Source) {
    let (text, enc) = decode(bytes, http);
    assert!(text.contains(want_text), "{name}: {text:?}");
    assert_eq!((enc.name, enc.source), (name, source));
}

#[test]
fn sniffing_order_and_decoding() {
    check(
        b"\xEF\xBB\xBF<p>caf\xC3\xA9</p>",
        Some("windows-1252"),
        "café",
        "UTF-8",
        Source::Bom,
    );
    let (u16, _, _) = encoding_rs::UTF_16LE.encode("<p>héllo</p>");
    let mut le = vec![0xFF, 0xFE];
    le.extend(u16.iter());
    // encoding_rs encodes UTF-16 output as UTF-8 (the Encoding standard has no UTF-16 encoder),
    // so build the bytes by hand.
    let le: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("<p>héllo</p>".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    check(&le, None, "héllo", "UTF-16LE", Source::Bom);
    let (sj, _, _) = encoding_rs::SHIFT_JIS.encode("<p>日本語</p>");
    check(&sj, Some("shift_jis"), "日本語", "Shift_JIS", Source::Http);
    let mut cyr = b"<meta charset=\"windows-1251\"><p>".to_vec();
    cyr.extend(encoding_rs::WINDOWS_1251.encode("Привет").0.iter());
    check(&cyr, None, "Привет", "windows-1251", Source::Meta);
    let mut eq =
        b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-2\"><p>".to_vec();
    eq.extend(encoding_rs::ISO_8859_2.encode("Łódź").0.iter());
    check(&eq, None, "Łódź", "ISO-8859-2", Source::Meta);
    check(
        b"<p>caf\xE9</p>",
        None,
        "café",
        "windows-1252",
        Source::Default,
    );
    let mut late = vec![b' '; 1100];
    late.extend(b"<meta charset=\"utf-8\"><p>caf\xE9</p>");
    check(&late, None, "café", "windows-1252", Source::Default);
}
