#!/usr/bin/env bash
# Structural proof that release.yml's no-op guard (task 1839) skips every
# build/host/upload/publish job when plan reports skip=true (a release for the
# tag already exists with assets). Cannot push a real tag from a test, so this
# propagates GitHub's job-skipping rules over the parsed workflow:
#   - a job whose `needs` contains a skipped job is skipped, unless its `if`
#     uses always() (then only its own condition decides);
#   - build-local-artifacts and host carry explicit guard terms on
#     plan.outputs.skip / publishing;
#   - announce requires host.result == 'success'.
# Stdlib-only python3 (no PyYAML: not a declared prerequisite).
# Usage: scripts/release-workflow.test.sh [workflow.yml]
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - "${1:-.github/workflows/release.yml}" <<'PY'
import sys, re

# Stdlib-only reader for the fixed shape of release.yml (no PyYAML: it is not a
# declared prerequisite). Understands exactly: top-level `jobs:`; job keys at 2
# spaces; per job single-line `if:`, `needs:` (inline or `- x` list), `outputs:`
# (single-line values) and `steps:` (`- ` items with id/name/run, run may be a
# `|` block). Anything else is ignored; a missing piece fails the asserts below.
def load(path):
    jobs, job, key, step = {}, None, None, None
    injobs = False
    for raw in open(path).read().split("\n"):
        if re.match(r"^\S", raw):
            injobs = raw.startswith("jobs:")
            continue
        if not injobs or not raw.strip() or raw.lstrip().startswith("#"):
            continue
        ind = len(raw) - len(raw.lstrip())
        t = raw.strip()
        if ind == 2 and t.endswith(":"):
            job = jobs.setdefault(t[:-1], {"steps": [], "outputs": {}})
            key = step = None
        elif job is None:
            continue
        elif ind == 4:
            m = re.match(r"([\w-]+):\s*(.*)$", t)
            key, step = (m.group(1) if m else None), None
            if m and m.group(1) == "if":
                job["if"] = m.group(2)
            elif m and m.group(1) == "needs" and m.group(2):
                job["needs"] = [m.group(2).strip("[] ")]
        elif key == "needs" and ind == 6 and t.startswith("- "):
            job.setdefault("needs", []).append(t[2:].strip())
        elif key == "outputs" and ind == 6:
            m = re.match(r"([\w-]+):\s*(.*)$", t)
            if m:
                job["outputs"][m.group(1)] = m.group(2)
        elif key == "steps":
            dash = ind == 6 and t.startswith("- ")
            if dash:
                step = {}
                job["steps"].append(step)
                t = t[2:]
            elif step is None:
                continue
            m = re.match(r"(id|name|run):\s*(.*)$", t) if (ind == 8 or dash) else None
            if m:
                step[m.group(1)] = m.group(2)
                step["_k"] = m.group(1) if m.group(1) == "run" else step.get("_k")
            elif step.get("_k") == "run" and ind > 8:
                step["run"] += "\n" + t
    return jobs

jobs = load(sys.argv[1])
assert "plan" in jobs, "no plan job parsed"
plan = jobs["plan"]
for s_ in plan["steps"]:
    s_.setdefault("id", None)

# 1. plan wires the guard.
steps = {s_["id"]: s_ for s_ in plan["steps"] if s_["id"]}
assert "existing" in steps, "plan has no `existing` guard step"
assert "release-exists.sh" in steps["existing"]["run"], "guard step does not call release-exists.sh"
assert "steps.existing.outputs.skip" in plan["outputs"]["skip"], "plan.outputs.skip not wired to the guard step"
assert "steps.existing.outputs.skip != 'true'" in plan["outputs"]["publishing"], "plan.outputs.publishing ignores the guard"
# guard must come before anything that builds or validates for publishing
order = [s_["id"] or s_.get("name") for s_ in plan["steps"]]
assert order.index("existing") < order.index("plan"), "guard must run before the dist plan step"

# 2. Propagate skip=true / publishing=false through the job graph.
def text(j): return str(jobs[j].get("if", ""))
ran = {"plan": True}
def needs(j):
    n = jobs[j].get("needs", [])
    return [n] if isinstance(n, str) else n
pending = [j for j in jobs if j != "plan"]
while pending:
    progressed = False
    for j in list(pending):
        if any(d not in ran for d in needs(j)):
            continue
        cond = text(j)
        up_skipped = any(not ran[d] for d in needs(j))
        if "always()" in cond:
            # only its own condition decides: it must demand a publish-side success
            runs = not ("publishing == 'true'" in cond or "needs.host.result == 'success'" in cond or "skip != 'true'" in cond)
        elif up_skipped:
            runs = False
        else:
            # direct child of plan: needs an explicit guard term
            # `publishing == 'true' || pr_run_mode == 'upload'` is NOT a guard by itself
            guarded = "skip != 'true'" in cond or ("publishing == 'true'" in cond and "pr_run_mode" not in cond)
            runs = not guarded
        ran[j] = runs
        pending.remove(j); progressed = True
    assert progressed, "cyclic or unresolved needs"

leaked = [j for j, r in ran.items() if r and j != "plan"]
assert not leaked, f"jobs that would still RUN when a release already exists: {leaked}"
print(f"release.yml skip scenario: plan runs; {len(ran)-1} of {len(jobs)-1} downstream jobs skipped: {sorted(j for j in ran if j != 'plan')}")
PY
