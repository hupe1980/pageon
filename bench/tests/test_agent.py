import pytest

from pageon_bench.agent import InvalidAction, parse, to_browsergym


@pytest.mark.parametrize("ident", ["e12", "12", "[ref=e12]"])
def test_parse_all_id_styles(ident):
    want = ident.replace("[ref=", "").strip("[]")
    assert parse(f"I will click.\nclick {ident}").ident == want
    a = parse(f'fill {ident} "a@b.c"')
    assert (a.verb, a.ident, a.arg) == ("fill", want, "a@b.c")
    assert parse(f'select {ident} "France"').arg == "France"
    assert parse(f"press {ident} Enter").arg == "Enter"


def test_scroll_and_done():
    assert parse("scroll down").arg == "down"
    assert parse("All set.\ndone").verb == "done"
    assert parse("```\nclick e3\n```").ident == "e3"


@pytest.mark.parametrize("reply", ["", "hmm", "click", "fill e1 no-quotes", "tap e1"])
def test_malformed(reply):
    with pytest.raises(InvalidAction):
        parse(reply)


def test_browsergym_strings_escape():
    a = parse('fill e1 "say \\"hi\\""')
    assert to_browsergym(a, "7").startswith('fill("7", ')


def test_tolerant_and_multi_action():
    from pageon_bench.agent import parse_all

    acts = parse_all('Let me fill it.\nfill ID "40" "Kasie"\npress ID "5" Enter')
    assert [(a.verb, a.ident, a.arg) for a in acts] == [("fill", "40", "Kasie"), ("press", "5", "Enter")]
    assert parse_all('click e1 "No"')[0].ident == "e1"
    assert parse_all("click ID e6")[0].ident == "e6"
    assert parse_all("click [ref=e6]\ndone")[1].verb == "done"


def test_coordinate_fallback_actions():
    from pageon_bench.agent import Action

    assert to_browsergym(Action("click", "e1"), (10.04, 20.0)) == "mouse_click(10.0, 20.0)"
    fill = to_browsergym(Action("fill", "e1", "hi"), (1.0, 2.0))
    assert fill.startswith("mouse_click(1.0, 2.0)") and 'keyboard_type("hi")' in fill
    assert to_browsergym(Action("select", "e1", "x"), (1.0, 2.0)) == "noop()"
