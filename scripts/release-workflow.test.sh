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
# Usage: scripts/release-workflow.test.sh [workflow.yml]
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - "${1:-.github/workflows/release.yml}" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf["jobs"]
plan = jobs["plan"]

# 1. plan wires the guard.
steps = {s.get("id"): s for s in plan["steps"] if s.get("id")}
assert "existing" in steps, "plan has no `existing` guard step"
assert "release-exists.sh" in steps["existing"]["run"], "guard step does not call release-exists.sh"
assert "steps.existing.outputs.skip" in plan["outputs"]["skip"], "plan.outputs.skip not wired to the guard step"
assert "steps.existing.outputs.skip != 'true'" in plan["outputs"]["publishing"], "plan.outputs.publishing ignores the guard"
# guard must come before anything that builds or validates for publishing
order = [s.get("id") or s.get("name") for s in plan["steps"]]
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
