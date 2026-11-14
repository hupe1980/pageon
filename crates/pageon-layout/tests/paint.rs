//! Paint: pixels from layout, images, frames, determinism.

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Images, Layout, LayoutOptions, Pixmap, Rect, Viewport};

fn with_images<'a>(images: &'a Images) -> LayoutOptions<'a> {
    LayoutOptions {
        images: Some(images),
        ..LayoutOptions::default()
    }
}

fn parse(html: &str) -> Dom {
    parse_document(html, ParseOptions::default())
}

fn by_id(dom: &Dom, id: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn viewport(l: &Layout) -> Rect {
    let v = l.viewport();
    Rect {
        x: 0.0,
        y: 0.0,
        width: v.width as f32,
        height: v.height as f32,
    }
}

fn paint(html: &str) -> (Dom, Pixmap) {
    let dom = parse(html);
    let mut l = Layout::new(&dom, Viewport::default(), &[], None);
    let px = l.paint(viewport(&l), 1.0).expect("paint");
    (dom, px)
}

fn close(p: [u8; 4], want: [u8; 3]) -> bool {
    p[..3].iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 8)
}

/// A solid PNG for image tests.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode");
    out
}

#[test]
fn boxes_text_and_borders_are_painted_where_layout_puts_them() {
    let (_, px) = paint(
        r#"<body style="margin:0">
        <div style="position:absolute;left:100px;top:50px;width:40px;height:30px;background:#ff0000"></div>
        <div style="position:absolute;left:300px;top:50px;width:40px;height:30px;border:4px solid #0000ff"></div>
        <p style="position:absolute;left:10px;top:200px;margin:0;font:40px sans-serif">Hello</p>"#,
    );
    assert_eq!((px.width, px.height), (1280, 720));
    assert!(close(px.pixel(120, 65).unwrap(), [255, 0, 0]), "red box");
    assert!(
        close(px.pixel(99, 65).unwrap(), [255, 255, 255]),
        "white outside"
    );
    assert!(
        close(px.pixel(301, 60).unwrap(), [0, 0, 255]),
        "blue border"
    );
    assert!(
        close(px.pixel(320, 65).unwrap(), [255, 255, 255]),
        "inside the border"
    );
    let dark = (10..120)
        .flat_map(|x| (200..250).map(move |y| (x, y)))
        .filter(|&(x, y)| px.pixel(x, y).unwrap()[0] < 100)
        .count();
    assert!(dark > 200, "text pixels: {dark}");
    assert!(
        close(px.pixel(5, 700).unwrap(), [255, 255, 255]),
        "white page"
    );
}

#[test]
fn areas_scale_and_errors() {
    let dom = parse(
        r#"<body style="margin:0"><div style="height:3000px"></div>
        <div style="position:absolute;left:0;top:2000px;width:50px;height:50px;background:#00ff00"></div>"#,
    );
    let mut l = Layout::new(&dom, Viewport::default(), &[], None);
    let (w, h) = l.document_size();
    assert_eq!(w, 1280.0);
    assert!(h >= 3000.0, "{h}");
    let clip = l
        .paint(
            Rect {
                x: 0.0,
                y: 1990.0,
                width: 100.0,
                height: 100.0,
            },
            2.0,
        )
        .unwrap();
    assert_eq!((clip.width, clip.height), (200, 200));
    assert!(
        close(clip.pixel(40, 60).unwrap(), [0, 255, 0]),
        "below the fold, at scale 2"
    );
    assert!(close(clip.pixel(40, 10).unwrap(), [255, 255, 255]));
    assert!(
        l.paint(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 10.0
            },
            1.0
        )
        .is_err()
    );
    assert!(
        l.paint(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20_000.0
            },
            1.0
        )
        .is_err()
    );
}

#[test]
fn repeat_paints_are_byte_identical() {
    let html = std::fs::read_to_string("../../bench/fixtures/pages/checkout.html").unwrap();
    let dom = parse(&html);
    let mut a = Layout::new(&dom, Viewport::default(), &[], None);
    let mut b = Layout::new(&dom, Viewport::default(), &[], None);
    let (pa, pb) = (
        a.paint(viewport(&a), 1.0).unwrap(),
        b.paint(viewport(&b), 1.0).unwrap(),
    );
    assert!(pa == pb);
    assert!(pa == a.paint(viewport(&a), 1.0).unwrap());
}

#[test]
fn images_load_on_demand_and_take_part_in_layout() {
    let dom = parse(
        r#"<body style="margin:0"><img id=i src="/a.png"><div id=after>x</div>
        <div id=bg style="width:20px;height:20px;background-image:url(/b.png)"></div>"#,
    );
    let base = Some("https://site.test/page");
    let mut images = Images::new();
    let l = Layout::with_options(&dom, Viewport::default(), &[], base, &with_images(&images));
    assert_eq!(
        l.wanted_images(),
        ["https://site.test/a.png", "https://site.test/b.png"]
    );
    // Without images, nothing is asked for and nothing changes.
    assert!(
        Layout::new(&dom, Viewport::default(), &[], base)
            .wanted_images()
            .is_empty()
    );

    // `srcset` alone: the 1x candidate.
    let only = parse(r#"<img srcset="/s2.png 2x, /s1.png 1x">"#);
    let l = Layout::with_options(&only, Viewport::default(), &[], base, &with_images(&images));
    assert_eq!(l.wanted_images(), ["https://site.test/s1.png"]);
    images
        .insert("https://site.test/a.png", &png(60, 40, [255, 0, 0]))
        .unwrap();
    images
        .insert("https://site.test/b.png", &png(4, 4, [0, 0, 255]))
        .unwrap();
    let mut l = Layout::with_options(&dom, Viewport::default(), &[], base, &with_images(&images));
    assert!(l.wanted_images().is_empty());
    assert_eq!(l.applied_images(), 2);
    let img = l.box_of(by_id(&dom, "i")).unwrap();
    assert_eq!((img.width, img.height), (60.0, 40.0), "natural size");
    let after = l.box_of(by_id(&dom, "after")).unwrap();
    assert!(after.y >= 40.0, "content moves below the image: {after:?}");
    let bg = l.box_of(by_id(&dom, "bg")).unwrap();
    let px = l.paint(viewport(&l), 1.0).unwrap();
    assert!(close(px.pixel(30, 20).unwrap(), [255, 0, 0]), "the image");
    assert!(
        close(
            px.pixel(bg.x as u32 + 2, bg.y as u32 + 2).unwrap(),
            [0, 0, 255]
        ),
        "the background image"
    );
}

#[test]
fn bad_images_fail_and_svg_images_render() {
    let mut images = Images::new();
    assert!(
        images
            .insert("https://s.test/x.png", b"not an image")
            .is_err()
    );
    assert!(images.failure("https://s.test/x.png").is_some());
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="30"><rect width="50" height="30" fill="#00ff00"/></svg>"##;
    assert_eq!(images.insert("https://s.test/i.svg", svg), Ok((50, 30)));
    let dom =
        parse(r#"<body style="margin:0"><img src="/i.svg"><img src="/x.png" width=10 height=10>"#);
    let mut l = Layout::with_options(
        &dom,
        Viewport::default(),
        &[],
        Some("https://s.test/"),
        &with_images(&images),
    );
    assert!(
        l.wanted_images().is_empty(),
        "failures are not asked for again"
    );
    let px = l.paint(viewport(&l), 1.0).unwrap();
    assert!(close(px.pixel(25, 15).unwrap(), [0, 255, 0]));
}

#[test]
fn frames_paint_inside_their_iframe_and_under_what_covers_them() {
    let dom = parse(
        r#"<body style="margin:0"><iframe id=f style="position:absolute;left:100px;top:100px;width:200px;height:100px;border:5px solid #000"></iframe>
        <div style="position:absolute;left:250px;top:100px;width:200px;height:200px;background:#0000ff"></div>"#,
    );
    let child_dom = parse(r#"<body style="margin:0;background:#ff0000">"#);
    let mut l = Layout::new(&dom, Viewport::default(), &[], None);
    let (w, h) = l.content_size(by_id(&dom, "f")).unwrap();
    assert_eq!((w, h), (200.0, 100.0));
    let child = Layout::new(
        &child_dom,
        Viewport {
            width: w as u32,
            height: h as u32,
        },
        &[],
        None,
    );
    assert!(l.embed(by_id(&dom, "f"), child));
    let px = l.paint(viewport(&l), 1.0).unwrap();
    assert!(
        close(px.pixel(150, 150).unwrap(), [255, 0, 0]),
        "the frame's document"
    );
    assert!(
        close(px.pixel(102, 150).unwrap(), [0, 0, 0]),
        "the iframe border"
    );
    assert!(
        close(px.pixel(280, 150).unwrap(), [0, 0, 255]),
        "the div above the frame"
    );
    assert!(
        close(px.pixel(150, 250).unwrap(), [255, 255, 255]),
        "outside"
    );
}

#[test]
fn controls_show_their_state_and_closed_details_hide_their_text() {
    let mut dom = parse(
        r#"<body style="margin:0;font:16px sans-serif">
        <select id=s style="position:absolute;left:10px;top:10px"><option>A</option><option id=g>Germany</option></select>
        <input id=i style="position:absolute;left:10px;top:60px">
        <details id=d style="position:absolute;left:10px;top:120px"><summary>More</summary>secret text</details>"#,
    );
    let g = by_id(&dom, "g");
    dom.control_mut(g).selected = Some(true);
    let i = by_id(&dom, "i");
    dom.control_mut(i).value = Some("typed-value".into());
    let mut l = Layout::new(&dom, Viewport::default(), &[], None);
    let s = l.box_of(by_id(&dom, "s")).unwrap();
    let px = l.paint(viewport(&l), 1.0).unwrap();
    let dark_in = |r: pageon_layout::Rect| {
        ((r.x as u32 + 3)..(r.x + r.width) as u32 - 3)
            .flat_map(|x| ((r.y as u32 + 3)..(r.y + r.height) as u32 - 3).map(move |y| (x, y)))
            .filter(|&(x, y)| px.pixel(x, y).unwrap()[0] < 128)
            .count()
    };
    assert!(
        dark_in(s) > 40,
        "the chosen option is painted inside the select"
    );
    assert!(
        dark_in(l.box_of(i).unwrap()) > 40,
        "the typed value is painted"
    );
    // Text in a closed <details> has no box (as in Chromium) and paints nothing.
    let d = l.box_of(by_id(&dom, "d")).unwrap();
    assert!(d.height < 25.0, "only the summary line: {d:?}");

    // Redaction: a secret never reaches pixels, in values or text.
    let secret = vec!["typed-value".to_string()];
    let opts = LayoutOptions {
        redact: &secret,
        ..LayoutOptions::default()
    };
    let mut r = Layout::with_options(&dom, Viewport::default(), &[], None, &opts);
    let redacted = r.paint(viewport(&r), 1.0).unwrap();
    assert!(redacted != px, "the value is drawn as bullets");
}

#[test]
#[ignore = "writes a PNG for eyeballing"]
fn eyeball() {
    let html = std::env::var("EYEBALL_HTML").unwrap_or_else(|_| "<p>x".into());
    let html = std::fs::read_to_string(&html).unwrap_or(html);
    let dom = parse(&html);
    let mut l = Layout::new(&dom, Viewport::default(), &[], None);
    let px = l.paint(viewport(&l), 1.0).unwrap();
    let out = std::env::var("EYEBALL_OUT").unwrap();
    image::RgbaImage::from_raw(px.width, px.height, px.data)
        .unwrap()
        .save(out)
        .unwrap();
}
