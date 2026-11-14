"""Episode runner: MiniWoB++ through BrowserGym, every format, resumable.

Every step: BrowserGym observes (and marks bids) → we capture the page with every baseline →
the format renders its observation → the model replies → the reply's id maps to a bid →
BrowserGym executes the action. Seeds are paired across formats: the same task instance is
played by every format.
"""

from __future__ import annotations

import fnmatch
import hashlib
import json
import os
import time
import traceback
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import __version__
from .agent import SYSTEM, InvalidAction, Model, parse_all, to_browsergym
from .capture import capture_page, read_capture, write_capture
from .formats import FORMATS, ActionError, live_baselines, make

RUNS = Path(__file__).resolve().parents[2] / "runs"
SMOKE_TASKS = ["click-test", "click-button", "enter-text", "login-user", "click-checkboxes"]


@dataclass(frozen=True)
class RunConfig:
    model: str
    endpoint: str
    formats: tuple[str, ...]
    tasks: tuple[str, ...]
    runs: int
    seed: int
    budget: int
    max_steps: int
    retries: int = 1
    suite: str = "miniwob"
    tool_version: str = __version__

    def key(self) -> str:
        d = asdict(self)
        d.pop("endpoint")
        return hashlib.sha256(json.dumps(d, sort_keys=True).encode()).hexdigest()[:12]


@dataclass
class EpisodeRecord:
    task: str
    format: str
    run_index: int
    seed: int
    success: bool = False
    reward: float = 0.0
    failure_cause: str | None = None
    steps: list[dict] = field(default_factory=list)
    invalid_actions: int = 0
    wall_ms: int = 0


def all_tasks() -> list[str]:
    import browsergym.miniwob  # noqa: F401 - registers the environments
    import gymnasium as gym

    prefix = "browsergym/miniwob."
    return sorted(k[len(prefix) :] for k in gym.envs.registry if k.startswith(prefix))


def _select(pattern: str) -> list[str]:
    if pattern == "smoke":
        return SMOKE_TASKS
    tasks = all_tasks()
    return tasks if pattern == "all" else [t for t in tasks if fnmatch.fnmatch(t, pattern)]


def _done(path: Path) -> set[tuple[str, str, int]]:
    done = set()
    if path.exists():
        for line in path.read_text().splitlines():
            try:
                r = json.loads(line)
                done.add((r["task"], r["format"], r["run_index"]))
            except (json.JSONDecodeError, KeyError):
                continue  # a torn last line from an interrupted run
    return done


def _append(path: Path, rec: EpisodeRecord) -> None:
    line = json.dumps(asdict(rec), ensure_ascii=False) + "\n"
    with path.open("a") as f:  # one write per episode; a torn line is skipped on resume
        f.write(line)
        f.flush()
        os.fsync(f.fileno())


def play(env_id: str, fmt_name: str, cfg: RunConfig, run_index: int, model: Model, out: Path) -> EpisodeRecord:
    import browsergym.miniwob  # noqa: F401 - registers the environments
    import gymnasium as gym

    seed = cfg.seed * 1000 + run_index
    rec = EpisodeRecord(task=env_id, format=fmt_name, run_index=run_index, seed=seed)
    t0 = time.monotonic()
    from browsergym.core.action.highlevel import HighLevelActionSet

    actions = HighLevelActionSet(subsets=["chat", "infeas", "bid", "nav", "tab", "coord"])
    env = gym.make(f"browsergym/miniwob.{env_id}", headless=True, action_mapping=actions.to_python_code)
    try:
        obs, _info = env.reset(seed=seed)
        page = env.unwrapped.page
        fmt = make(fmt_name, cfg.budget)
        goal = obs.get("goal") or ""
        messages = [{"role": "system", "content": SYSTEM.format(x=fmt.example_id)}]
        nav, url, prefix = 0, page.url, ""
        for step in range(cfg.max_steps):
            if page.url != url:
                nav, url = nav + 1, page.url
            baselines, extras = live_baselines(page, obs.get("axtree_object"))
            cap = capture_page(page, step=step, nav_counter=nav, baselines=baselines, extras=extras)
            cpath = out / "captures" / env_id / fmt_name / str(run_index) / f"step-{step}.capture.json.gz"
            write_capture(cap, cpath)
            observation = fmt.observe(cap, cpath)
            content = (f"Task: {goal}\n\n" if step == 0 else "") + (prefix + "\n" if prefix else "") + observation
            messages.append({"role": "user", "content": content})
            reply, usage = model.act(messages)
            messages.append({"role": "assistant", "content": reply})
            srec = {
                "capture": str(cpath.relative_to(out)),
                "observation_tokens": _count(observation),
                "reply": reply,
                **asdict(usage),
            }
            try:
                actions = parse_all(reply)
            except InvalidAction as e:
                rec.invalid_actions += 1
                srec["outcome"] = "invalid_action"
                prefix = fmt.note_action("act", "?", str(e)) or f"err: {e}"
                rec.steps.append(srec)
                continue
            srec["actions"] = [asdict(a) for a in actions]
            outcomes, finished, notes = [], False, []
            for action in actions:
                if action.verb == "done":
                    finished = True
                    break
                bid = None
                if action.verb != "scroll":
                    try:
                        bid = fmt.to_bid(action.ident, cap, cpath, page)
                    except ActionError as e:
                        rec.invalid_actions += 1
                        outcomes.append("unknown_ref")
                        notes.append(fmt.note_action(action.verb, action.ident, str(e)))
                        break
                obs, reward, terminated, truncated, _info = env.step(to_browsergym(action, bid))
                err = obs.get("last_action_error") or None
                outcomes.append("error" if err else "ok")
                notes.append(fmt.note_action(action.verb, action.ident or action.arg, err))
                rec.reward = float(reward)
                if terminated or truncated or err:
                    finished = finished or terminated or truncated
                    break
            srec["outcome"] = outcomes[-1] if outcomes else "done"
            srec["outcomes"] = outcomes
            prefix = "\n".join(n for n in notes if n)
            rec.steps.append(srec)
            if finished:
                break
        rec.success = rec.reward > 0
    except Exception as e:
        rec.failure_cause = f"{type(e).__name__}: {e}"
        rec.success = False
        (out / "errors.log").open("a").write(f"{env_id} {fmt_name} {run_index}\n{traceback.format_exc()}\n")
    finally:
        try:
            env.close()
        except Exception:
            pass
    rec.wall_ms = int((time.monotonic() - t0) * 1000)
    return rec


def _count(text: str) -> int:
    import pageon

    return pageon.count_tokens(text)


def main(args) -> int:
    from . import report
    from .cli import check_endpoint, miniwob_url

    os.environ.setdefault("MINIWOB_URL", miniwob_url())
    check_endpoint(args.endpoint)
    formats = tuple(args.formats or FORMATS)
    for f in formats:
        if f not in FORMATS:
            raise SystemExit(f"error: unknown format {f!r}; choose from {', '.join(FORMATS)}")
    cfg = RunConfig(
        model=args.model,
        endpoint=args.endpoint,
        formats=formats,
        tasks=tuple(_select(args.tasks)),
        runs=args.runs,
        seed=args.seed,
        budget=args.budget,
        max_steps=args.max_steps,
    )
    out = Path(args.run_dir) if args.run_dir else RUNS / f"{'smoke' if args.smoke else 'run'}-{cfg.key()}"
    out.mkdir(parents=True, exist_ok=True)
    model = Model(args.endpoint, args.model, args.seed)
    meta = {**asdict(cfg), "model_hash": model.model_hash(), "started": time.strftime("%Y-%m-%dT%H:%M:%S")}
    (out / "config.json").write_text(json.dumps(meta, indent=2))
    episodes = out / "episodes.jsonl"
    done = _done(episodes)
    planned = [(t, f, r) for r in range(cfg.runs) for t in cfg.tasks for f in cfg.formats]
    todo = [p for p in planned if p not in done]
    print(
        f"run {out.name}: {len(planned)} episodes ({len(done)} done, {len(todo)} to go) · "
        f"{len(cfg.tasks)} tasks × {len(formats)} formats × {cfg.runs} runs · model {cfg.model}"
    )
    deadline = time.monotonic() + args.max_hours * 3600
    for i, (task, fmt, run_index) in enumerate(todo, 1):
        if time.monotonic() > deadline:
            print("wall-clock limit reached; the report marks the run incomplete (INCONCLUSIVE)")
            break
        rec = play(task, fmt, cfg, run_index, model, out)
        _append(episodes, rec)
        mark = "✓" if rec.success else ("!" if rec.failure_cause else "✗")
        print(
            f"  [{i}/{len(todo)}] {mark} {task:28} {fmt:15} run {run_index} "
            f"{len(rec.steps)} steps {rec.wall_ms / 1000:.1f}s"
        )
    report.write(out)
    verdict = report.gate_verdict(out)
    print(f"✓ {out / 'report.md'} · gate {verdict}")
    return 0


__all__ = ["EpisodeRecord", "RunConfig", "main", "play", "read_capture"]
