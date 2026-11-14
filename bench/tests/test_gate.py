from pageon_bench.stats import verdict


def test_pass_by_ci():
    assert verdict(0.01, 0.10, r=1.0) == "PASS"


def test_pass_by_tokens():
    assert verdict(-0.01, 0.05, r=0.5) == "PASS"


def test_fail_by_ci():
    assert verdict(-0.10, -0.01, r=0.3) == "FAIL"


def test_fail_by_tokens():
    assert verdict(-0.01, 0.05, r=0.9) == "FAIL"


def test_inconclusive_wide_interval():
    assert verdict(-0.05, 0.08, r=0.9) == "INCONCLUSIVE"
    assert verdict(-0.05, 0.08, r=0.3) == "INCONCLUSIVE"


def test_incomplete_run_is_inconclusive():
    assert verdict(-0.01, 0.05, r=0.9, complete=False) == "INCONCLUSIVE"


def test_incomplete_never_decides():
    assert verdict(0.1, 0.2, r=0.1, complete=False) == "INCONCLUSIVE"
    assert verdict(-0.3, -0.2, r=0.9, complete=False) == "INCONCLUSIVE"
