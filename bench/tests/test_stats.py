from pageon_bench import stats


def test_bootstrap_is_deterministic_and_brackets_mean():
    v = [1, 0, 1, 1, 0, 1, 1, 1, 0, 1]
    a = stats.ci(v)
    assert a == stats.ci(v)
    assert a[1] <= a[0] <= a[2]


def test_edges():
    assert stats.ci([1.0] * 20) == (1.0, 1.0, 1.0)
    assert stats.ci([0.0] * 20) == (0.0, 0.0, 0.0)
    m, lo, hi = stats.ci([0.5])
    assert m == lo == hi == 0.5


def test_per_task_nests_runs():
    eps = [{"task": "a", "success": True}, {"task": "a", "success": False}, {"task": "b", "success": True}]
    assert stats.per_task(eps) == {"a": 0.5, "b": 1.0}


def test_paired_diff_uses_shared_tasks():
    d, lo, hi = stats.paired_diff({"a": 1.0, "b": 1.0, "c": 0.0}, {"a": 0.0, "b": 1.0})
    assert d == 0.5 and lo <= d <= hi


def test_extra_runs_needed():
    assert stats.extra_runs_needed(0.05, -0.05, 0.15, 3) > 0
    assert stats.extra_runs_needed(0.2, 0.1, 0.3, 3) is None
