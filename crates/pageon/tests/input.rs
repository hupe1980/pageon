//! Input as a person gives it: mouse buttons, click counts, modifiers, key chords and
//! sequences, held keys, typing into the focused element, the wheel at a point.

mod common;

use common::*;
use pageon::{Action, Budget, Dump, Replay, Session, Viewport};
use pageon_env::Env;

const PAGE: &str = "<style>#b{position:absolute;left:0;top:0;width:300px;height:100px}</style>
<button id=b>Target</button>
<div style='height:120px'></div>
<a id=l href=/next>Next page</a>
<input id=f aria-label=Field value=old>
<input id=g aria-label=Other>
<p id=log>LOG|</p>
<script>
const log = t => document.getElementById('log').textContent += t + ';';
const b = document.getElementById('b');
for (const t of ['mousedown','mouseup','click','dblclick','auxclick','contextmenu'])
  b.addEventListener(t, e => log(t + ':' + e.button + ':' + e.detail + (e.ctrlKey ? ':ctrl' : '') + (e.shiftKey ? ':shift' : '')));
b.addEventListener('contextmenu', e => e.preventDefault());
document.addEventListener('keydown', e => { if (e.target.id === 'g') log('kd:' + e.key + (e.repeat ? ':r' : '') + (e.ctrlKey ? ':ctrl' : '')); });
</script>";

fn text<E: Env>(tab: &pageon::Tab<'_, E>) -> String {
    tab.view(Dump::Text, Viewport::default(), Budget::o200k(4000))
}

fn act<E: Env>(tab: &mut pageon::Tab<'_, E>, a: &str) -> pageon::Outcome {
    tab.act(a.parse().unwrap_or_else(|e| panic!("{a}: {e}")))
        .unwrap_or_else(|e| panic!("{a}: {e}"))
}

fn log_of<E: Env>(tab: &pageon::Tab<'_, E>) -> String {
    let t = text(tab);
    t.split("LOG|")
        .nth(1)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn open(log: pageon_env::Log) -> Session<Replay> {
    Session::new(Replay::new(log))
}

#[test]
fn buttons_counts_and_modifiers() {
    let s = open(site(vec![html("https://in.test/", PAGE)]));
    let mut tab = s
        .open("https://in.test/", corpus_options())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let b = find(&obs, "button", "Target");
    act(&mut tab, &format!("click {b} double"));
    assert_eq!(
        log_of(&tab),
        "mousedown:0:1;mouseup:0:1;click:0:1;mousedown:0:2;mouseup:0:2;click:0:2;dblclick:0:2;"
    );
    act(&mut tab, &format!("click {b} right"));
    act(&mut tab, "click 50,50 middle ctrl");
    let l = log_of(&tab);
    assert!(
        l.ends_with("mousedown:2:1;contextmenu:2:1;mouseup:2:1;auxclick:2:1;mousedown:1:1:ctrl;mouseup:1:1:ctrl;auxclick:1:1:ctrl;"),
        "{l}"
    );
    // Recordings keep the click's kind.
    let a: Action = "click 50,50 triple shift"
        .parse()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a.to_string(), "click 50,50 triple shift");
    assert_eq!(
        serde_json::from_value::<Action>(serde_json::to_value(&a).unwrap_or_default()).ok(),
        Some(a)
    );
}

#[test]
fn modifier_clicks_on_links_open_tabs() {
    let s = open(site(vec![html("https://in.test/", PAGE)]));
    let mut tab = s
        .open("https://in.test/", corpus_options())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let l = find(&obs, "link", "Next page");
    for how in ["ctrl", "meta", "middle"] {
        let o = act(&mut tab, &format!("click {l} {how}"));
        assert_eq!(o.navigated, None, "{how}");
        assert_eq!(o.popups, vec!["https://in.test/next"], "{how}");
    }
}

#[test]
fn chords_sequences_and_holds() {
    let s = open(site(vec![html("https://in.test/", PAGE)]));
    let mut tab = s
        .open("https://in.test/", corpus_options())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let f = find(&obs, "textbox", "Field");
    act(&mut tab, &format!("click {f}"));
    // Select all, then typing replaces the value.
    act(&mut tab, "press ctrl+a");
    act(&mut tab, "type \"new\"");
    assert!(
        tab.observe_full().contains("=\"new\""),
        "{}",
        tab.observe_full()
    );
    // Typing appends at the end; a sequence with a repeat; xdotool names.
    act(&mut tab, "type \"er\"");
    act(&mut tab, "press BackSpace x2 Return");
    assert!(tab.observe_full().contains("=\"new\""));
    // shift+Tab moves focus back; Tab forward.
    act(&mut tab, "press Tab");
    act(&mut tab, "press ctrl+b");
    let held = act(&mut tab, "hold ctrl 600");
    assert!(
        held.settled.virtual_ms >= 600,
        "{}",
        held.settled.virtual_ms
    );
    let l = log_of(&tab);
    assert!(l.contains("kd:b:ctrl"), "{l}");
    assert!(l.contains("kd:Control;kd:Control:r"), "{l}");
    act(&mut tab, "press shift+Tab");
    act(&mut tab, "type \"x\"");
    assert!(
        tab.observe_full().contains("=\"newx\""),
        "{}",
        tab.observe_full()
    );
    // Unknown keys are refused by name.
    let e = tab
        .act("press hyper+q".parse().unwrap_or_else(|e| panic!("{e}")))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("hyper+q"), "{e}");
}

#[test]
fn typing_with_nothing_focused_says_so() {
    let s = open(site(vec![html("https://in.test/", PAGE)]));
    let mut tab = s
        .open("https://in.test/", corpus_options())
        .unwrap_or_else(|e| panic!("{e}"));
    let o = act(&mut tab, "type \"hi\"");
    assert!(o.unfocused);
}

#[test]
fn the_wheel_turns_where_the_pointer_is() {
    let page = "<div id=box style='height:100px;overflow:auto' aria-label=Box>\
                <div style='height:1000px'>tall</div></div><div style='height:3000px'></div>";
    let s = open(site(vec![html("https://in.test/", page)]));
    let mut tab = s
        .open("https://in.test/", corpus_options())
        .unwrap_or_else(|e| panic!("{e}"));
    let o = act(&mut tab, "scroll 20,20 down 300");
    assert_eq!(o.scrolled, vec!["scrolled a container to 0,300"]);
    let o = act(&mut tab, "scroll 20,500 down 300");
    assert_eq!(o.scrolled, vec!["scrolled to 0,300"]);
}
