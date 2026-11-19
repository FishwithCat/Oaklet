#!/usr/bin/env python3
"""Repeatable parallel pilot for the code-index rename benefit test.

The pilot asks a real model to rename `regex_syntax::hir::Look` to `LookKind`
in a pinned checkout of rust-lang/regex.  Each attempt gets its own git
worktree, so attempts cannot see each other's edits.  The `base` arm uses the
production tool set; the `code` arm additionally registers the four code-index
tools.  A run passes only when:

  * `cargo check` succeeds for the touched crates,
  * the `Look` enum in regex-syntax was actually renamed, and
  * the unrelated `Look` types in regex-automata and regex-lite were left
    alone.

The script writes specs, model transcripts, patches, compile logs, and a
Markdown/JSON summary under the output directory.  It never prints or stores
the API key in a spec; the key is passed to the ignored Rust probe through
`OAKLET_BENCH_API_KEY`.

Example:

    python3 scripts/bench/rename_pilot.py \
        --repo /path/to/rust-lang/regex \
        --runs 16 --jobs 4 \
        --cargo-home /path/to/cargo-home

The Rust probe is `packages/core/src/agent/bench.rs`
(`agent::bench::benefit_pilot`).  It is compiled once, then the test binary is
invoked directly so cargo's target lock does not serialize the parallel runs.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import json
import math
import os
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CORE = REPO_ROOT / "packages" / "core"

PINNED_REV = "fde4946d4a3cb367338a21493ec91ec7106a12b4"

TASK = {
    "name": "look",
    "prompt": (
        "Rename the `Look` type in `regex_syntax::hir` to `LookKind` throughout "
        "this workspace, updating its definition and every use. Keep behavior "
        "unchanged. When you are done, reply with the list of files you changed."
    ),
    "ready": {
        "path": "regex-syntax/src/hir/mod.rs",
        "line": 1613,
        "column": 12,
    },
    "definition_file": "regex-syntax/src/hir/mod.rs",
    "definition_marker": "pub enum LookKind",
    "packages": ["regex-syntax", "regex-automata", "regex", "regex-lite"],
    # A consistent rename of these would still compile, but it is the wrong
    # symbol.  They must stay untouched.
    "protected": [
        "regex-automata/src/util/look.rs",
        "regex-automata/src/nfa/thompson/builder.rs",
        "regex-lite/",
    ],
}


def log(message: str) -> None:
    print(message, flush=True)


def run_capture(cmd, cwd, env, timeout):
    started = time.monotonic()
    try:
        result = subprocess.run(
            [str(part) for part in cmd],
            cwd=cwd,
            env=env,
            text=True,
            capture_output=True,
            timeout=timeout,
        )
        return {
            "rc": result.returncode,
            "out": result.stdout,
            "err": result.stderr,
            "timed_out": False,
            "seconds": time.monotonic() - started,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "rc": -9,
            "out": error.stdout or "",
            "err": (error.stderr or "") + "\n[timeout]",
            "timed_out": True,
            "seconds": time.monotonic() - started,
        }


def run_logged(cmd, cwd, env, log_path, timeout):
    """Run a command in its own process group and stream output to a log."""
    started = time.monotonic()
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("w") as sink:
        process = subprocess.Popen(
            [str(part) for part in cmd],
            cwd=cwd,
            env=env,
            stdout=sink,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        try:
            rc = process.wait(timeout=timeout)
            timed_out = False
        except subprocess.TimeoutExpired:
            timed_out = True
            rc = -9
            try:
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        return {
            "rc": rc,
            "timed_out": timed_out,
            "seconds": time.monotonic() - started,
        }


def git(repo, *args, check=True):
    result = subprocess.run(
        ["git", "-C", str(repo), *map(str, args)],
        text=True,
        capture_output=True,
    )
    if check and result.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(map(str, args))} failed: {result.stderr.strip()}"
        )
    return result


def prepare_repo(args, out_dir):
    if args.repo:
        repo = Path(args.repo).expanduser().resolve()
    else:
        repo = out_dir / "regex"
        if not repo.exists():
            log(f"cloning rust-lang/regex into {repo}")
            result = subprocess.run(
                [
                    "git",
                    "clone",
                    "--quiet",
                    "https://github.com/rust-lang/regex",
                    str(repo),
                ],
                text=True,
                capture_output=True,
            )
            if result.returncode != 0:
                sys.exit(f"git clone failed: {result.stderr.strip()}")

    if not (repo / ".git").exists():
        sys.exit(f"{repo} is not a git checkout")

    if git(repo, "cat-file", "-e", f"{args.rev}^{{commit}}", check=False).returncode:
        log(f"fetching {args.rev}")
        result = git(repo, "fetch", "--quiet", "origin", args.rev, check=False)
        if result.returncode:
            sys.exit(f"revision {args.rev} is not available in {repo}")

    head = git(repo, "rev-parse", "HEAD").stdout.strip()
    if head != args.rev:
        log(f"note: {repo} HEAD is {head[:12]}, worktrees pin {args.rev[:12]}")
    return repo


def build_probe_binary(args, env):
    if args.binary:
        binary = Path(args.binary).expanduser().resolve()
        if not binary.exists():
            sys.exit(f"probe binary does not exist: {binary}")
        return binary

    log("building probe test binary once")
    result = run_capture(
        [
            args.cargo,
            "test",
            "--lib",
            "--no-run",
            "--offline",
            "--locked",
            "--message-format=json",
        ],
        cwd=CORE,
        env=env,
        timeout=1800,
    )
    if result["rc"] != 0:
        sys.exit(
            "cargo test --no-run failed:\n"
            + (result["err"] or result["out"])[-4000:]
        )
    for line in result["out"].splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        executable = item.get("executable")
        if item.get("reason") == "compiler-artifact" and executable:
            return Path(executable)
    sys.exit("could not find the test binary in cargo's JSON output")


def add_worktree(repo, tree, rev):
    if tree.exists():
        git(repo, "worktree", "remove", "--force", tree, check=False)
        shutil.rmtree(tree, ignore_errors=True)
    result = git(repo, "worktree", "add", "--detach", "--force", tree, rev, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"git worktree add failed: {result.stderr.strip()}")


def changed_files(tree):
    result = git(tree, "status", "--porcelain", check=False)
    changed = []
    for line in result.stdout.splitlines():
        if len(line) < 4:
            continue
        path = line[3:]
        if " -> " in path:
            path = path.split(" -> ", 1)[1]
        path = path.strip().strip('"')
        if path in ("Cargo.lock",) or path.startswith("target/"):
            continue
        changed.append(path)
    return changed


def save_state(tree, run_dir, run_id):
    changed = changed_files(tree)
    patch = git(tree, "diff", "--binary", check=False).stdout
    patch_path = run_dir / "patches" / f"{run_id}.diff"
    patch_path.parent.mkdir(parents=True, exist_ok=True)
    patch_path.write_text(patch)
    return changed, patch_path


def reset_worktree(tree, rev):
    git(tree, "reset", "--hard", rev, check=False)
    git(tree, "clean", "-fdq", check=False)


def write_spec(path, args, tree, arm, result_path, config):
    spec = {
        "workspace": str(tree),
        "group": arm,
        "task": TASK["name"],
        "prompt": TASK["prompt"],
        "out": str(result_path),
        "baseUrl": config["baseUrl"],
        "model": config["model"],
        "maxSteps": args.max_steps,
        "readyPosition": TASK["ready"],
    }
    if config.get("contextWindow"):
        spec["contextWindow"] = config["contextWindow"]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(spec, ensure_ascii=False, indent=1))


def load_agent_result(path):
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def compile_run(args, out_dir, check_env, run):
    """Check one worktree in its own target directory.

    Cargo's dep-info records paths relative to the worktree, so sharing a
    target directory between worktrees makes every run after the first look
    fresh: their files are older than the first run's artifacts. Each run
    therefore gets an isolated target directory.
    """
    env_check = check_env.copy()
    target = out_dir / "target-check" / run["id"]
    shutil.rmtree(target, ignore_errors=True)
    env_check["CARGO_TARGET_DIR"] = str(target)
    package_args = []
    for package in TASK["packages"]:
        package_args += ["-p", package]
    result = run_capture(
        [args.cargo, "check", *package_args, "--offline"],
        cwd=run["tree"],
        env=env_check,
        timeout=1200,
    )
    text = (result["out"] + "\n" + result["err"]).strip()
    (out_dir / "checks" / f"{run['id']}.log").write_text(text)
    return result, text


def score_run(run, compile_result, compile_text):
    result = run.get("result")
    protected = [
        path
        for path in run["changed"]
        if any(
            path == item or (item.endswith("/") and path.startswith(item))
            for item in TASK["protected"]
        )
    ]
    definition_ok = False
    definition_path = run["tree"] / TASK["definition_file"]
    if definition_path.exists():
        definition_ok = TASK["definition_marker"] in definition_path.read_text(
            errors="replace"
        )

    errors = [
        line
        for line in compile_text.splitlines()
        if line.startswith("error[") or line.startswith("error:")
    ]
    agent_error = (result or {}).get("error")
    if run.get("timed_out"):
        reason = "agent_timeout"
    elif agent_error:
        reason = "agent_error"
    elif compile_result["rc"] != 0 or compile_result["timed_out"]:
        reason = "compile_error"
    elif not definition_ok:
        reason = "definition_not_renamed"
    elif protected:
        reason = "protected_edit"
    else:
        reason = "pass"

    steps = (result or {}).get("steps") or []
    tools = [step.get("tool", "") for step in steps]
    usage = (result or {}).get("usage") or {}
    valid = reason not in ("agent_error", "agent_timeout")
    return {
        "reason": reason,
        "success": reason == "pass",
        "valid": valid,
        "changed": run["changed"],
        "protected": protected,
        "definition_ok": definition_ok,
        "compile_rc": compile_result["rc"],
        "compile_timed_out": compile_result["timed_out"],
        "compile_errors": len(errors),
        "first_errors": errors[:3],
        "code_calls": sum(1 for name in tools if name.startswith("code_")),
        "reference_calls": sum(1 for name in tools if name == "code_references"),
        "tool_calls": len(tools),
        "tokens": usage.get("total", 0),
        "prompt_tokens": usage.get("prompt", 0),
        "completion_tokens": usage.get("completion", 0),
        "duration_s": ((result or {}).get("durationMs") or int(run["seconds"] * 1000))
        / 1000,
        "ready_s": ((result or {}).get("readyMs") or 0) / 1000,
        "agent_error": agent_error,
        "steps": len(steps),
    }


def wilson(successes, total, z=1.96):
    if total == 0:
        return (0.0, 0.0)
    phat = successes / total
    denominator = 1 + z * z / total
    center = (phat + z * z / (2 * total)) / denominator
    margin = (
        z
        * math.sqrt(phat * (1 - phat) / total + z * z / (4 * total * total))
        / denominator
    )
    return (max(0.0, center - margin), min(1.0, center + margin))


def fisher_two_sided(a, b, c, d):
    """Two-sided Fisher exact p for [[a, b], [c, d]]."""
    n = a + b + c + d
    row1 = a + b
    col1 = a + c
    denominator = math.comb(n, row1)

    def probability(x):
        return math.comb(col1, x) * math.comb(n - col1, row1 - x) / denominator

    observed = probability(a)
    low = max(0, row1 - (n - col1))
    high = min(row1, col1)
    return sum(
        probability(x)
        for x in range(low, high + 1)
        if probability(x) <= observed + 1e-12
    )


def summarize(rows, args, arms, out_dir, repo, config):
    lines = []
    lines.append("# regex `Look` rename pilot")
    lines.append("")
    lines.append(f"- date: {datetime.datetime.now().isoformat(timespec='seconds')}")
    lines.append(f"- repo: `{repo}` @ `{args.rev}`")
    lines.append(f"- model: `{config['model']}`")
    lines.append(f"- runs per arm: {args.runs}; jobs: {args.jobs}")
    lines.append(
        f"- task: rename `regex_syntax::hir::Look` to `LookKind`; keep "
        "regex-automata's own `Look` and regex-lite untouched"
    )
    lines.append("")

    lines.append("## Per run")
    lines.append("")
    lines.append(
        "| run | valid | pass | reason | changed | compile errors | code_* | refs | "
        "tokens | seconds | ready |"
    )
    lines.append(
        "| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )
    for row in rows:
        lines.append(
            f"| {row['id']} | {'yes' if row['score']['valid'] else 'no'} | "
            f"{'yes' if row['score']['success'] else 'no'} | "
            f"{row['score']['reason']} | {len(row['score']['changed'])} | "
            f"{row['score']['compile_errors']} | {row['score']['code_calls']} | "
            f"{row['score']['reference_calls']} | {row['score']['tokens']} | "
            f"{row['score']['duration_s']:.1f} | {row['score']['ready_s']:.1f} |"
        )

    lines.append("")
    lines.append("## Per arm")
    lines.append("")
    lines.append(
        "| arm | valid n | invalid | pass | rate | 95% Wilson CI | mean tokens | "
        "median seconds | code_* / run | refs / run |"
    )
    lines.append(
        "| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |"
    )
    counts = {}
    for arm in arms:
        group = [
            row for row in rows if row["arm"] == arm and row["score"]["valid"]
        ]
        invalid = [
            row for row in rows if row["arm"] == arm and not row["score"]["valid"]
        ]
        if not group:
            lines.append(f"| {arm} | 0 | {len(invalid)} | 0/0 | – | – | – | – | – | – |")
            continue
        passed = sum(1 for row in group if row["score"]["success"])
        counts[arm] = (passed, len(group) - passed)
        low, high = wilson(passed, len(group))
        lines.append(
            f"| {arm} | {len(group)} | {len(invalid)} | {passed}/{len(group)} | "
            f"{passed / len(group):.1%} | {low:.1%}–{high:.1%} | "
            f"{statistics.mean(row['score']['tokens'] for row in group):.0f} | "
            f"{statistics.median(row['score']['duration_s'] for row in group):.1f} | "
            f"{statistics.mean(row['score']['code_calls'] for row in group):.1f} | "
            f"{statistics.mean(row['score']['reference_calls'] for row in group):.1f} |"
        )

    if "base" in counts and "code" in counts:
        base_pass, base_fail = counts["base"]
        code_pass, code_fail = counts["code"]
        p_value = fisher_two_sided(base_pass, base_fail, code_pass, code_fail)
        lines.append("")
        lines.append(
            f"Two-sided Fisher exact test on pass/fail: p = {p_value:.3g}."
        )

    failures = [
        row for row in rows if row["score"]["valid"] and not row["score"]["success"]
    ]
    invalid = [row for row in rows if not row["score"]["valid"]]
    lines.append("")
    lines.append("## Failures")
    lines.append("")
    if not failures:
        lines.append("None.")
    for row in failures:
        score = row["score"]
        lines.append(
            f"- **{row['id']}** `{score['reason']}`: "
            f"{score['agent_error'] or '; '.join(score['first_errors']) or 'no detail'}"
        )
        if score["changed"]:
            lines.append(f"  changed: {', '.join(score['changed'][:8])}")
        if score["protected"]:
            lines.append(f"  protected: {', '.join(score['protected'])}")

    if invalid:
        lines.append("")
        lines.append("## Invalid runs (excluded from arm statistics)")
        lines.append("")
        for row in invalid:
            score = row["score"]
            lines.append(
                f"- **{row['id']}** `{score['reason']}`: "
                f"{score['agent_error'] or 'no detail'}"
            )

    lines.append("")
    lines.append("## Reproduce")
    lines.append("")
    lines.append("```bash")
    command = (
        f"python3 scripts/bench/rename_pilot.py --repo {repo} "
        f"--runs {args.runs} --jobs {args.jobs} --rev {args.rev}"
    )
    if args.resume:
        command += f" --resume {out_dir}"
    if args.cargo_home:
        command += f" --cargo-home {args.cargo_home}"
    lines.append(command)
    lines.append("```")
    lines.append("")
    lines.append(f"Artifacts: `{out_dir}`.")
    lines.append("")
    return "\n".join(lines)


def recheck(args, out_dir, config):
    """Re-run compile checks for an existing run from its saved patches."""
    payload = json.loads((out_dir / "summary.json").read_text())
    saved = dict(payload.get("args") or {})
    saved.setdefault("recheck", True)
    saved["recheck"] = True
    run_args = argparse.Namespace(**saved)
    for key in ("repo", "rev", "cargo", "cargo_home", "check_jobs", "keep_worktrees"):
        value = getattr(args, key)
        if value is not None:
            setattr(run_args, key, value)
    run_args.resume = str(out_dir)
    run_args.out_dir = str(out_dir)
    arms = [arm.strip() for arm in run_args.arms.split(",") if arm.strip()]

    repo = prepare_repo(run_args, out_dir)
    check_env = os.environ.copy()
    check_env["CARGO_NET_OFFLINE"] = "true"
    if run_args.cargo_home:
        check_env["CARGO_HOME"] = str(Path(run_args.cargo_home).expanduser())

    rows = []
    for item in payload.get("runs", []):
        run_id = item["id"]
        tree = out_dir / "trees" / run_id
        add_worktree(repo, tree, run_args.rev)
        patch = out_dir / "patches" / f"{run_id}.diff"
        if patch.exists() and patch.stat().st_size:
            applied = git(tree, "apply", "--whitespace=nowarn", patch, check=False)
            if applied.returncode != 0:
                raise RuntimeError(
                    f"cannot apply {patch} for {run_id}: {applied.stderr.strip()}"
                )
        changed = changed_files(tree)
        rows.append(
            {
                "id": run_id,
                "arm": item["arm"],
                "tree": tree,
                "changed": changed,
                "result": load_agent_result(out_dir / "results" / f"{run_id}.json"),
                "seconds": item.get("seconds", 0),
                "exit": item.get("exit", 0),
            }
        )

    def check(run):
        result, text = compile_run(run_args, out_dir, check_env, run)
        run["compile"] = result
        run["compile_text"] = text
        run["score"] = score_run(run, result, text)
        return run

    with concurrent.futures.ThreadPoolExecutor(
        max_workers=max(1, run_args.check_jobs)
    ) as pool:
        rows = list(pool.map(check, rows))

    rows.sort(key=lambda row: (arms.index(row["arm"]), row["id"]))
    summary = summarize(rows, run_args, arms, out_dir, repo, config)
    (out_dir / "summary.md").write_text(summary)
    payload["args"] = vars(run_args)
    payload["model"] = config["model"]
    payload["generatedAt"] = datetime.datetime.now().isoformat(timespec="seconds")
    payload["runs"] = [
        {
            "id": run["id"],
            "arm": run["arm"],
            "seconds": run.get("seconds", run["score"]["duration_s"]),
            "exit": run.get("exit", 0),
            **run["score"],
        }
        for run in rows
    ]
    (out_dir / "summary.json").write_text(
        json.dumps(payload, ensure_ascii=False, indent=2)
    )
    print(summary)

    if not run_args.keep_worktrees:
        for run in rows:
            git(repo, "worktree", "remove", "--force", run["tree"], check=False)
        git(repo, "worktree", "prune", check=False)
        log(f"removed worktrees; artifacts kept in {out_dir}")
    return 0


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", help="rust-lang/regex checkout; cloned if omitted")
    parser.add_argument("--rev", default=PINNED_REV)
    parser.add_argument("--runs", type=int, default=16, help="attempts per arm")
    parser.add_argument("--arms", default="base,code")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--out-dir", help="default: .data/bench/rename-pilot/<stamp>")
    parser.add_argument("--config", help="Oaklet config JSON")
    parser.add_argument("--base-url")
    parser.add_argument("--model")
    parser.add_argument("--max-steps", type=int, default=60)
    parser.add_argument("--timeout", type=int, default=1800, help="seconds per attempt")
    parser.add_argument("--retries", type=int, default=2, help="retries for transient HTTP errors")
    parser.add_argument("--retry-delay", type=int, default=30)
    parser.add_argument("--check-jobs", type=int, default=1)
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    parser.add_argument("--cargo-home", default=os.environ.get("CARGO_HOME"))
    parser.add_argument("--binary", help="prebuilt agent::bench::benefit_pilot binary")
    parser.add_argument("--keep-worktrees", action="store_true")
    parser.add_argument(
        "--recheck",
        action="store_true",
        help="with --resume, re-run compile checks from saved patches instead of "
        "calling the model again",
    )
    parser.add_argument(
        "--resume",
        help="existing output directory; keep its valid runs and only run the missing ones",
    )
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args(argv)


def load_config(args):
    path = args.config
    if not path:
        path = os.environ.get("OAKLET_BENCH_CONFIG")
    if not path:
        path = Path.home() / "Library/Application Support/com.oaklet.desktop/config.json"
    path = Path(path).expanduser()
    if not path.exists():
        sys.exit(f"config not found: {path} (pass --config)")
    config = json.loads(path.read_text())
    for key in ("baseUrl", "apiKey", "model"):
        if not config.get(key):
            sys.exit(f"config {path} is missing {key}")
    if args.base_url:
        config["baseUrl"] = args.base_url
    if args.model:
        config["model"] = args.model
    return config


def main(argv=None):
    args = parse_args(argv)
    arms = [arm.strip() for arm in args.arms.split(",") if arm.strip()]
    if not arms or any(arm not in ("base", "code", "code_nudge", "code_only") for arm in arms):
        sys.exit(f"invalid --arms: {args.arms}")
    if args.runs < 1:
        sys.exit("--runs must be positive")
    if args.jobs < 1:
        sys.exit("--jobs must be positive")

    prior = {}
    if args.resume:
        out_dir = Path(args.resume).expanduser().resolve()
        summary_path = out_dir / "summary.json"
        if not summary_path.exists():
            sys.exit(f"cannot resume without {summary_path}")
        for item in json.loads(summary_path.read_text()).get("runs", []):
            reason = item.get("reason", "")
            valid = item.get(
                "valid", reason not in ("agent_error", "agent_timeout")
            )
            item["valid"] = valid
            if valid:
                prior[item["id"]] = item
    else:
        stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
        out_dir = (
            Path(args.out_dir).expanduser().resolve()
            if args.out_dir
            else REPO_ROOT / ".data" / "bench" / "rename-pilot" / stamp
        )
        if out_dir.exists() and any(out_dir.iterdir()):
            sys.exit(f"output directory is not empty: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    for name in ("results", "logs", "specs", "patches", "checks", "trees"):
        (out_dir / name).mkdir(exist_ok=True)

    config = load_config(args)
    log(
        f"pilot: {args.runs} x {', '.join(arms)} attempts, jobs={args.jobs}, "
        f"model={config['model']}"
    )
    log(f"artifacts: {out_dir}")

    if args.recheck:
        if not args.resume:
            sys.exit("--recheck requires --resume <existing output directory>")
        return recheck(args, out_dir, config)

    if args.dry_run:
        for arm in arms:
            for index in range(1, args.runs + 1):
                log(f"would run {arm}-{index:02d}")
        return 0

    # Building the probe uses the checkout's own Cargo home (usually ~/.cargo).
    # The regex worktrees may need a different, pre-populated Cargo home, so
    # only the compile-check phase applies --cargo-home.
    env = os.environ.copy()
    build_env = env.copy()
    build_env["CARGO_NET_OFFLINE"] = "true"
    check_env = build_env.copy()
    if args.cargo_home:
        check_env["CARGO_HOME"] = str(Path(args.cargo_home).expanduser())
    repo = prepare_repo(args, out_dir)
    binary = build_probe_binary(args, build_env)
    log(f"probe binary: {binary}")

    runs = []
    for index in range(1, args.runs + 1):
        for arm in arms:
            runs.append(
                {
                    "id": f"{TASK['name']}-{arm}-{index:02d}",
                    "arm": arm,
                    "index": index,
                    "tree": out_dir / "trees" / f"{arm}-{index:02d}",
                }
            )

    if prior:
        kept = 0
        for run in runs:
            if run["id"] in prior:
                run["done"] = True
                run["prior_score"] = prior[run["id"]]
                kept += 1
        log(f"resume: keeping {kept} valid runs, running {len(runs) - kept} more")

    pending = [run for run in runs if not run.get("done")]
    log(f"creating {len(pending)} worktrees at {args.rev[:12]}")
    for run in pending:
        add_worktree(repo, run["tree"], args.rev)
        run["spec"] = out_dir / "specs" / f"{run['id']}.json"
        run["result_path"] = out_dir / "results" / f"{run['id']}.json"
        run["log"] = out_dir / "logs" / f"{run['id']}.log"
        write_spec(run["spec"], args, run["tree"], run["arm"], run["result_path"], config)

    stop = threading.Event()
    consecutive_errors = 0
    lock = threading.Lock()

    def execute(run):
        nonlocal consecutive_errors
        if run.get("done"):
            return run
        if stop.is_set():
            return {**run, "skipped": True}
        log(f"start {run['id']}")
        run_env = env.copy()
        run_env["OAKLET_BENCH"] = str(run["spec"])
        run_env["OAKLET_BENCH_API_KEY"] = config["apiKey"]
        attempts = 0
        elapsed = 0.0
        while True:
            attempts += 1
            outcome = run_logged(
                [binary, "agent::bench::benefit_pilot", "--ignored", "--nocapture"],
                cwd=CORE,
                env=run_env,
                log_path=out_dir / "logs" / f"{run['id']}-try{attempts}.log",
                timeout=args.timeout,
            )
            elapsed += outcome["seconds"]
            run["result"] = load_agent_result(run["result_path"])
            error = (run["result"] or {}).get("error") or ""
            transient = any(
                marker in error
                for marker in (
                    "429",
                    "HTTP 500",
                    "HTTP 502",
                    "HTTP 503",
                    "HTTP 504",
                    "error sending request",
                    "error decoding response body",
                )
            )
            if transient and attempts <= args.retries:
                log(
                    f"retry {run['id']} after transient error "
                    f"({attempts}/{args.retries}): {error[:120]}"
                )
                time.sleep(args.retry_delay * attempts)
                reset_worktree(run["tree"], args.rev)
                continue
            break
        run["timed_out"] = outcome["timed_out"]
        run["seconds"] = elapsed
        run["exit"] = outcome["rc"]
        run["attempts"] = attempts
        changed, patch = save_state(run["tree"], out_dir, run["id"])
        run["changed"] = changed
        run["patch"] = patch

        error = (run["result"] or {}).get("error")
        with lock:
            fatal = bool(error) and any(
                marker in error
                for marker in ("insufficient", "HTTP 401", "HTTP 403")
            )
            if fatal:
                stop.set()
                log(f"stopping: fatal agent error: {error[:160]}")
            elif error:
                consecutive_errors += 1
                if consecutive_errors >= 8:
                    stop.set()
                    log("stopping: 8 consecutive agent errors")
            else:
                consecutive_errors = 0
        log(
            f"done  {run['id']} in {elapsed:.0f}s "
            f"({len(changed)} changed){' ERROR: ' + error if error else ''}"
        )
        return run

    executed = []
    try:
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
                futures = [pool.submit(execute, run) for run in runs]
                for future in concurrent.futures.as_completed(futures):
                    executed.append(future.result())
        finally:
            log("checking edits")
            env_check = check_env.copy()

            def check(run):
                if run.get("skipped") or run.get("done"):
                    return run
                result, text = compile_run(args, out_dir, env_check, run)
                run["compile"] = result
                run["compile_text"] = text
                return run

            with concurrent.futures.ThreadPoolExecutor(
                max_workers=max(1, args.check_jobs)
            ) as pool:
                checked = list(pool.map(check, executed))

            rows = []
            for run in checked:
                if run.get("skipped"):
                    continue
                if run.get("done"):
                    run["score"] = run["prior_score"]
                else:
                    run["score"] = score_run(run, run["compile"], run["compile_text"])
                rows.append(run)
            rows.sort(key=lambda row: (arms.index(row["arm"]), row["id"]))

            summary = summarize(rows, args, arms, out_dir, repo, config)
            (out_dir / "summary.md").write_text(summary)
            payload = {
                "args": vars(args),
                "rev": args.rev,
                "repo": str(repo),
                "model": config["model"],
                "generatedAt": datetime.datetime.now().isoformat(timespec="seconds"),
                "runs": [
                    {
                        "id": run["id"],
                        "arm": run["arm"],
                        "seconds": run.get("seconds", run["score"]["duration_s"]),
                        "exit": run.get("exit", 0),
                        **run["score"],
                    }
                    for run in rows
                ],
            }
            (out_dir / "summary.json").write_text(
                json.dumps(payload, ensure_ascii=False, indent=2)
            )
            print(summary)

            if not args.keep_worktrees:
                for run in runs:
                    git(repo, "worktree", "remove", "--force", run["tree"], check=False)
                git(repo, "worktree", "prune", check=False)
                log(f"removed worktrees; artifacts kept in {out_dir}")
            else:
                log(f"kept worktrees under {out_dir / 'trees'}")
    finally:
        if not args.keep_worktrees:
            for run in runs:
                if run["tree"].exists():
                    shutil.rmtree(run["tree"], ignore_errors=True)
            git(repo, "worktree", "prune", check=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())
