//! Drag and drop: trusted pointer drags, HTML drag and drop with `DataTransfer`,
//! drop zones, in-process and caged.

mod common;

use common::*;
use pageon::{
    Action, Budget, Dump, Files, LoadOptions, Replay, Sandbox, SandboxOptions, Session, Viewport,
};
use pageon_env::Env;

const BOARD: &str = "<style>section{display:block;min-height:80px;margin:8px}</style>
<section id=todo aria-label=Todo><div id=c1 role=button draggable=true>Write report</div></section>
<section id=done aria-label=Done></section>
<p id=log></p>
<script>
const log = t => document.getElementById('log').textContent += t + ';';
const card = document.getElementById('c1');
card.addEventListener('dragstart', e => {
  e.dataTransfer.setData('text/plain', card.id);
  e.dataTransfer.effectAllowed = 'move';
  log('start ' + e.dataTransfer.types.join());
});
for (const col of document.querySelectorAll('section')) {
  col.addEventListener('dragover', e => {
    if (!col.dataset.seen) { col.dataset.seen = 1; log('over ' + col.id + ' [' + e.dataTransfer.getData('text/plain') + '] ' + e.dataTransfer.types.join()); }
    if (col.id === 'done') e.preventDefault();
  });
  col.addEventListener('drop', e => {
    e.preventDefault();
    const id = e.dataTransfer.getData('text/plain');
    col.append(document.getElementById(id));
    log('drop ' + id + ' on ' + col.id);
  });
}
document.addEventListener('dragend', e => log('end ' + e.dataTransfer.dropEffect));
document.addEventListener('mouseup', () => log('mouseup'));
</script>";

fn text_of_page<E: Env>(tab: &pageon::Tab<'_, E>) -> String {
    tab.view(Dump::Text, Viewport::default(), Budget::o200k(4000))
}

fn board() -> pageon_env::Log {
    site(vec![html("https://board.test/", BOARD)])
}

fn drag<E: Env>(
    tab: &mut pageon::Tab<'_, E>,
    from: (&str, &str),
    to: (&str, &str),
) -> pageon::Outcome {
    let obs = tab.observe_full();
    let a = find(&obs, from.0, from.1);
    let b = find(&obs, to.0, to.1);
    tab.act(
        format!("drag {a} {b}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_card_moves_with_html_drag_and_drop() {
    let session = Session::new(Replay::new(board()));
    let mut tab = session
        .open("https://board.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = drag(&mut tab, ("button", "Write report"), ("droptarget", "Done"));
    assert!(out.to_string().contains("dragged (dropped)"), "{out}");
    let t = text_of_page(&tab);
    // Data is readable at the start and the drop, protected in between (types only).
    assert!(t.contains("start text/plain;"), "{t}");
    assert!(t.contains("over done [] text/plain;"), "{t}");
    assert!(t.contains("drop c1 on done;end move;"), "{t}");
    assert!(!t.contains("mouseup"), "the drag took the pointer: {t}");
    let obs = tab.observe_full();
    let done = obs
        .split("region")
        .find(|p| p.contains("\"Done\""))
        .unwrap_or_default();
    assert!(done.contains("Write report"), "{obs}");
}

#[test]
fn a_target_that_does_not_accept_gets_no_drop() {
    let session = Session::new(Replay::new(board()));
    let mut tab = session
        .open("https://board.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = drag(&mut tab, ("button", "Write report"), ("droptarget", "Todo"));
    assert!(out.to_string().contains("not accepted"), "{out}");
    let t = text_of_page(&tab);
    assert!(!t.contains("drop"), "{t}");
    assert!(t.contains("end none;"), "{t}");
}

#[test]
fn pointer_driven_lists_reorder() {
    let page = "<style>li{display:block;height:40px}</style><ul id=list>
      <li role=button id=a>Alpha</li><li role=button id=b>Beta</li><li role=button id=c>Gamma</li></ul><p id=plog></p>
      <script>
      let dragging = null, moves = 0;
      list.addEventListener('pointerdown', e => { dragging = e.target; });
      document.addEventListener('pointermove', e => { if (dragging && e.buttons === 1) moves++; });
      document.addEventListener('pointerup', e => {
        if (!dragging) return;
        const over = e.target;
        if (over !== dragging && over.parentNode === list) list.insertBefore(dragging, over.nextSibling);
        plog.textContent = 'moves ' + moves + ' up on ' + over.id + ' order ' + [...list.children].map(x => x.id).join('');
        dragging = null;
      });
      list.addEventListener('click', () => plog.textContent += ' click');
      </script>";
    let session = Session::new(Replay::new(site(vec![html("https://list.test/", page)])));
    let mut tab = session
        .open("https://list.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = drag(&mut tab, ("button", "Alpha"), ("button", "Gamma"));
    assert!(out.to_string().contains("pointer drag"), "{out}");
    let t = text_of_page(&tab);
    assert!(t.contains("moves 4 up on c order bca"), "{t}");
    assert!(!t.contains("click"), "a drag is not a click: {t}");
}

#[test]
fn a_cancelled_dragstart_stays_a_pointer_drag() {
    let page = "<div role=button draggable=true id=s>Source</div><div role=button id=t>Target</div><p id=o></p>
      <script>s.addEventListener('dragstart', e => e.preventDefault());
      document.addEventListener('mouseup', e => o.textContent = 'mouseup on ' + e.target.id);</script>";
    let session = Session::new(Replay::new(site(vec![html("https://c.test/", page)])));
    let mut tab = session
        .open("https://c.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = drag(&mut tab, ("button", "Source"), ("button", "Target"));
    assert!(out.to_string().contains("pointer drag"), "{out}");
    assert!(
        text_of_page(&tab).contains("mouseup on t"),
        "{}",
        text_of_page(&tab)
    );
}

#[test]
fn data_transfer_builds_file_lists() {
    // What Playwright's setInputFiles does over connectOverCDP.
    let page = "<input type=file id=f><p id=o></p><script>
      const dt = new DataTransfer();
      dt.items.add(new File(['hi there'], 'a.txt', {type: 'text/plain'}));
      dt.items.add('x', 'text/plain');
      let dup; try { dt.items.add('y', 'text/plain'); } catch (e) { dup = e.name; }
      f.files = dt.files;
      o.textContent = [dt.items.length, dt.types.join('+'), f.files.length, f.files[0].name,
        dt.items[0].kind, dt.items[1].type, dt.getData('text'), dup, dt.dropEffect, dt.effectAllowed,
        new DragEvent('drop', {dataTransfer: dt}).dataTransfer === dt].join(',');
    </script>";
    let session = Session::new(Replay::new(site(vec![html("https://d.test/", page)])));
    let tab = session
        .open("https://d.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        text_of_page(&tab).contains(
            "2,text/plain+Files,1,a.txt,file,text/plain,x,NotSupportedError,none,none,true"
        ),
        "{}",
        text_of_page(&tab)
    );
}

const ZONE: &str = "<div id=zone role=region aria-label='Drop files'>Drop here</div><p id=got></p><script>
  zone.addEventListener('dragover', e => e.preventDefault());
  zone.addEventListener('drop', async e => {
    e.preventDefault();
    const f = e.dataTransfer.files[0];
    got.textContent = f.name + ' ' + f.size + ' ' + (await f.text()) + ' ' + e.dataTransfer.types.join();
  });
</script>";

fn staged(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pageon-drag-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let p = dir.join("notes.txt");
    std::fs::write(&p, "dropped bytes").unwrap_or_else(|e| panic!("{e}"));
    p
}

#[test]
fn files_drop_on_drop_zones() {
    let env = Files::new(Replay::new(site(vec![html("https://zone.test/", ZONE)]))).with(
        "notes",
        staged("inproc"),
        ["https://zone.test"],
    );
    let session = Session::new(env);
    let mut tab = session
        .open("https://zone.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let zone = find(&obs, "droptarget", "Drop files");
    let out = tab
        .act(Action::Upload(zone.clone(), vec!["notes".into()]))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.to_string().contains("dragged (dropped)"), "{out}");
    assert!(
        text_of_page(&tab).contains("notes.txt 13 dropped bytes Files"),
        "{}",
        text_of_page(&tab)
    );
    // Only staged names.
    let e = tab
        .act(Action::Upload(zone, vec!["passwd".into()]))
        .map(|o| o.to_string())
        .unwrap_or_else(|e| e.to_string());
    assert!(e.contains("not staged") && e.contains("--file"), "{e}");
}

fn caged<E: pageon::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn caged_drags_and_drops() {
    let mut sb = caged(Replay::new(board()));
    let mut tab = sb
        .open("https://board.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let a = find(&obs, "button", "Write report");
    let b = find(&obs, "droptarget", "Done");
    let out = tab
        .act(Action::Drag(a, b))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.to_string().contains("dragged (dropped)"), "{out}");
    let env = Files::new(Replay::new(site(vec![html("https://zone.test/", ZONE)]))).with(
        "notes",
        staged("caged"),
        ["https://zone.test"],
    );
    let mut sb = caged(env);
    let mut tab = sb
        .open("https://zone.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let zone = find(&obs, "droptarget", "Drop files");
    tab.act(Action::Upload(zone, vec!["notes".into()]))
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("notes.txt 13 dropped bytes Files"), "{obs}");
}
