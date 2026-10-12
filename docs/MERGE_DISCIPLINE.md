# Merge discipline

`tools/ws.sh` mechanizes the two things `BRANCHING.md` and `TESTING.md` already
ask for, so they happen the same way every time instead of by memory:

- **Worktree-isolated work.** Each change is built on its own branch in its own
  git worktree off `main`, never by editing `main` or `stable` directly.
- **A test gate before every merge.** A merge is never kept unless the gate
  passes on the *merged* tree. Red leaves the target branch exactly as it was.
- **Builder claims checked, not trusted.** "I made N commits and tests pass" is
  checked against the branch: the commits must exist, the worktree must be
  clean, and a gate pass must be recorded for the exact HEAD commit.

Nothing here replaces `BRANCHING.md`/`TESTING.md`; it enforces them. It adds no
dependencies (bash + git + cargo), pushes nothing, and never promotes to
`stable` on steps a machine cannot verify.

## The gate is only as real as this machine

The promotion checklist has three steps. What `ws.sh` can *automate* depends on
what is installed:

| Step (TESTING.md) | Needs | `ws gate` runs it when... |
|---|---|---|
| 1. Builds clean (zero warnings) | `cargo` (+ nightly) | always |
| 2. QEMU smoke test | `qemu-system-x86_64`, `xorriso` | both are on PATH |
| 3. VirtualBox boot | a person | never — attested by hand |

Run `ws doctor` to see which gate this box gives you. On a box without QEMU +
xorriso (e.g. this Windows checkout), the gate is **build-only**, and a merge to
`stable` is refused unless you attest the steps you ran elsewhere
(`--i-ran-smoke --i-ran-vbox`). On a Linux/WSL box with QEMU + xorriso, step 2
runs automatically as part of the gate.

> rose needs a **nightly** toolchain (the `limine` crate uses an unstable
> feature) but pins none. `ws.sh` uses `+nightly` automatically when it is
> installed; override with `ROSE_CARGO_TOOLCHAIN`.

## The loop

```bash
tools/ws.sh new hats                 # worktree .worktrees/hats on branch wt/hats, off main
# ... build in .worktrees/hats, commit there ...
tools/ws.sh gate .worktrees/hats     # run the gate; records a PASS marker on HEAD
tools/ws.sh check hats               # commits ahead? clean? gate backed? -> claims stand
tools/ws.sh merge hats               # trial-merge into main, gate the merged tree, keep on green
tools/ws.sh drop hats                # remove the worktree + branch when done
```

Promoting `main` to `stable` (after the QEMU + VirtualBox steps):

```bash
tools/ws.sh merge main --to stable --i-ran-smoke --i-ran-vbox
# then paste the printed row into docs/BRANCHING.md's merge log
```

Wait — `merge main --to stable` expects a `wt/main`. For the real promotion you
merge the current `main` tip; see "Promoting to stable" below for the exact
form once you've picked how you want main→stable to run.

## If a merge goes wrong

Merges are never pushed by `ws.sh`, so undo is local:

```bash
tools/ws.sh rollback main --to-sha <pre-merge-sha>     # the sha merge printed
tools/ws.sh rollback stable --revert <merge-sha>       # if it was already pushed
```

## Rolling back the discipline itself

This whole thing lives on branch `wt/merge-discipline` and is three files
(`tools/ws.sh`, this doc, the `.gitignore` line for `.worktrees/`). If it is a
nuisance: don't merge the branch, or `git revert` the merge, or delete the
files. It changes no existing code and no existing script.
