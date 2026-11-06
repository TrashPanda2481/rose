#!/usr/bin/env bash
# ws.sh - merge discipline for rose: worktree-isolated work, a test gate before
# every merge, and builder claims checked against the branch instead of trusted.
#
# It mechanizes the main -> stable model in docs/BRANCHING.md and the promotion
# checklist in docs/TESTING.md. It never pushes and never touches stable without
# an explicit human attestation of the steps a machine here cannot run.
#
# Subcommands (see `ws.sh help`):
#   doctor                 what this machine can gate, and what it cannot
#   new <name>             a worktree + branch wt/<name> off main to work in
#   gate [<dir>]           run the gate on a tree; PASS/FAIL, exit 0/1
#   check <name>           check a builder's claims against branch wt/<name>
#   merge <name> [--to main|stable] [attestations]
#                          gated merge: trial-merge, gate the MERGED tree,
#                          keep on green, undo on red. target untouched on fail.
#   rollback <target> --to-sha <sha> | --revert <merge-sha>
#                          undo a merge (reset if unpushed, revert if pushed)
#   drop <name>            remove the worktree and delete branch wt/<name>
set -uo pipefail

# --- locate the repo (works from the main checkout or any worktree) -----------
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# primary worktree = the main checkout; branch-mutating ops run there.
PRIMARY="$(git -C "$SELF_DIR" worktree list --porcelain 2>/dev/null \
  | awk '/^worktree /{print substr($0,10); exit}')"
GIT_COMMON="$(git -C "$SELF_DIR" rev-parse --git-common-dir 2>/dev/null)"
if [ -z "$PRIMARY" ] || [ -z "$GIT_COMMON" ]; then
  echo "ws: not inside the rose git repository" >&2; exit 2
fi
WT_ROOT="$PRIMARY/.worktrees"
SCRATCH="$PRIMARY/.scratch"
GATE_DB="$SCRATCH/gate"       # per-commit gate markers: <sha>.pass / <sha>.log

c_red=$'\033[31m'; c_grn=$'\033[32m'; c_ylw=$'\033[33m'; c_dim=$'\033[2m'; c_off=$'\033[0m'
say()  { printf '%s\n' "$*"; }
ok()   { printf '%sok%s   %s\n'   "$c_grn" "$c_off" "$*"; }
warn() { printf '%swarn%s %s\n'   "$c_ylw" "$c_off" "$*"; }
fail() { printf '%sFAIL%s %s\n'   "$c_red" "$c_off" "$*"; }
die()  { printf '%sws: %s%s\n' "$c_red" "$*" "$c_off" >&2; exit 2; }

have() { command -v "$1" >/dev/null 2>&1; }

# resolve a name to a branch: prefer the worktree branch wt/<name>, else a
# plain branch <name> (so `merge main --to stable` works too). prints the
# branch name; returns 1 if neither exists.
resolve_branch() {
  local n="$1"
  if git -C "$PRIMARY" show-ref --verify --quiet "refs/heads/wt/$n"; then printf 'wt/%s' "$n"; return 0; fi
  if git -C "$PRIMARY" show-ref --verify --quiet "refs/heads/$n"; then printf '%s' "$n"; return 0; fi
  return 1
}

# cargo toolchain: rose needs nightly (the limine crate uses an unstable
# feature) but pins none. Prefer an explicit override, else nightly if present.
cargo_tc() {
  if [ -n "${ROSE_CARGO_TOOLCHAIN:-}" ]; then printf '%s' "$ROSE_CARGO_TOOLCHAIN"; return; fi
  if rustup toolchain list 2>/dev/null | grep -q '^nightly'; then printf '+nightly'; fi
}

# which gate can run on THIS machine
gate_kind() {
  if have qemu-system-x86_64 && have xorriso && [ -x "$PRIMARY/tools/smoke-test.sh" ]; then
    printf 'smoke'
  else
    printf 'build'
  fi
}

# ---------------------------------------------------------------------------
# gate: run the detected gate inside <dir> (default .). writes a log and, on a
# pass, a per-commit marker so a later `check` can prove the gate actually ran
# on this exact commit. returns 0 on PASS.
# ---------------------------------------------------------------------------
cmd_gate() {
  local mark=1
  if [ "${1:-}" = --no-mark ]; then mark=0; shift; fi   # merge uses this: a
  # trial merge leaves HEAD at the pre-merge commit, so a marker here would be
  # mis-keyed; the merge trusts the exit code, not the marker.
  local dir="${1:-.}"
  dir="$(cd "$dir" 2>/dev/null && pwd)" || die "no such directory: ${1:-.}"
  local kind; kind="$(gate_kind)"
  local sha; sha="$(git -C "$dir" rev-parse HEAD 2>/dev/null || echo unknown)"
  mkdir -p "$GATE_DB"
  local log="$GATE_DB/${sha}.log"
  if [ "$mark" = 1 ]; then
    say "${c_dim}gate: $kind  tree: $dir  head: ${sha:0:12}${c_off}"
  else
    say "${c_dim}gate: $kind  merged tree in $dir (uncommitted)${c_off}"
  fi

  local rc
  if [ "$kind" = smoke ]; then
    ( cd "$dir" && ./tools/smoke-test.sh ) >"$log" 2>&1; rc=$?
  else
    local tc; tc="$(cargo_tc)"
    ( cd "$dir/kernel" && cargo $tc build --release ) >"$log" 2>&1; rc=$?
    # their bar is zero warnings (BRANCHING.md); treat warnings as a failure
    # unless ROSE_GATE_ALLOW_WARN=1.
    if [ "$rc" -eq 0 ] && [ "${ROSE_GATE_ALLOW_WARN:-0}" != 1 ]; then
      if grep -qE '^warning' "$log"; then
        rc=3
        printf '\n[ws] build succeeded but emitted warnings (zero-warning bar)\n' >>"$log"
      fi
    fi
  fi

  tail -n 12 "$log" | sed 's/^/    /'
  [ "$mark" = 1 ] && rm -f "$GATE_DB/${sha}.pass"
  if [ "$rc" -eq 0 ]; then
    local on=""; [ "$mark" = 1 ] && { printf '%s %s %s\n' "$kind" "$(date -u +%FT%TZ)" "$sha" >"$GATE_DB/${sha}.pass"; on=" on ${sha:0:12}"; }
    ok "gate PASS ($kind)${on}  log: ${log/#$PRIMARY\//}"
    return 0
  fi
  case "$rc" in
    3) fail "gate FAIL ($kind): build has warnings. log: ${log/#$PRIMARY\//}";;
    *) fail "gate FAIL ($kind) rc=$rc. log: ${log/#$PRIMARY\//}";;
  esac
  return 1
}

# ---------------------------------------------------------------------------
# new: isolate work in a worktree + branch off main.
# ---------------------------------------------------------------------------
cmd_new() {
  local name="${1:-}"; [ -n "$name" ] || die "usage: ws new <name>"
  local path="$WT_ROOT/$name" br="wt/$name"
  [ -e "$path" ] && die "worktree already exists: ${path/#$PRIMARY\//}"
  git -C "$PRIMARY" show-ref --verify --quiet "refs/heads/$br" \
    && die "branch $br already exists (drop it first, or pick another name)"
  git -C "$PRIMARY" worktree add "$path" -b "$br" main >/dev/null \
    || die "could not create worktree"
  ok "worktree $br at ${path/#$PRIMARY\//}  (off main)"
  say "   build here, commit here; then:  ws check $name  &&  ws merge $name"
}

# ---------------------------------------------------------------------------
# check: a builder's claims, verified against the branch - not trusted.
#   - branch exists, worktree present and CLEAN (no uncommitted 'done')
#   - commits actually ahead of main (so 'N commits' is checkable)
#   - a gate PASS marker exists for the exact branch HEAD (so 'tests pass'
#     is backed by a gate run on that commit, not asserted)
# exits non-zero if the claim cannot stand.
# ---------------------------------------------------------------------------
cmd_check() {
  local name="" base=main
  while [ $# -gt 0 ]; do case "$1" in
    --base) base="${2:-}"; shift 2;;
    -*) die "check: unknown option $1";;
    *) name="$1"; shift;;
  esac; done
  [ -n "$name" ] || die "usage: ws check <name> [--base <branch>]"
  local br; br="$(resolve_branch "$name")" || { fail "no branch wt/$name or $name"; return 2; }
  local path="$WT_ROOT/$name"
  local bad=0
  say "check: $br (base $base)"
  ok "branch exists"

  if [ -d "$path" ]; then
    if [ -z "$(git -C "$path" status --porcelain)" ]; then ok "worktree clean"
    else fail "worktree has uncommitted changes (a 'done' with loose edits)"; bad=1
      git -C "$path" status --porcelain | sed 's/^/     /'
    fi
  else
    warn "no worktree at ${path/#$PRIMARY\//} (branch-only check)"
  fi

  local n; n="$(git -C "$PRIMARY" rev-list --count "$base..$br" 2>/dev/null || echo 0)"
  if [ "$n" -gt 0 ]; then
    ok "$n commit(s) ahead of $base:"
    git -C "$PRIMARY" log --oneline --no-decorate "$base..$br" | sed 's/^/     /'
  else
    fail "no commits ahead of $base (nothing to merge)"; bad=1
  fi

  local sha; sha="$(git -C "$PRIMARY" rev-parse "$br" 2>/dev/null)"
  if [ -f "$GATE_DB/${sha}.pass" ]; then
    ok "gate PASS recorded on HEAD ${sha:0:12}: $(cat "$GATE_DB/${sha}.pass")"
  else
    warn "no gate PASS on HEAD ${sha:0:12} - claim 'tests pass' is UNBACKED"
    say  "     run:  ws gate ${path/#$PRIMARY\//}  (merge will gate regardless; this is advisory)"
  fi

  if [ "$bad" -eq 0 ]; then ok "claims stand"; return 0; fi
  fail "claims do NOT stand"; return 1
}

# ---------------------------------------------------------------------------
# merge: the gate decides. trial-merge wt/<name> into the target, gate the
# MERGED tree, commit only on green; on red or conflict the target is left
# exactly as it was. does not push.
# ---------------------------------------------------------------------------
cmd_merge() {
  local name="" target=main attest_smoke=0 attest_vbox=0
  while [ $# -gt 0 ]; do case "$1" in
    --to) target="${2:-}"; shift 2;;
    --i-ran-smoke) attest_smoke=1; shift;;
    --i-ran-vbox)  attest_vbox=1; shift;;
    -*) die "merge: unknown option $1";;
    *) name="$1"; shift;;
  esac; done
  [ -n "$name" ] || die "usage: ws merge <name> [--to main|stable]"
  case "$target" in main|stable) ;; *) die "merge: --to must be main or stable";; esac

  local src; src="$(resolve_branch "$name")" || die "no branch wt/$name or $name"
  [ "$src" = "$target" ] && die "source and target are the same branch ($target)"
  # must run where the target branch lives (the primary checkout).
  local here; here="$(git rev-parse --show-toplevel 2>/dev/null)"
  [ "$here" = "$PRIMARY" ] || die "run merge from the main checkout: $PRIMARY"
  [ -z "$(git -C "$PRIMARY" status --porcelain)" ] || die "main checkout is dirty; commit or stash first"

  # claims must stand before we even try.
  cmd_check "$name" --base "$target" >/dev/null 2>&1 || { cmd_check "$name" --base "$target"; die "fix the above before merging"; }

  # stable needs the steps this box cannot run; require explicit attestation.
  if [ "$target" = stable ]; then
    local kind; kind="$(gate_kind)"
    if [ "$attest_smoke" != 1 ] && [ "$kind" != smoke ]; then
      die "stable needs the QEMU smoke test (TESTING.md step 2); not runnable here.
     run it on a box with qemu+xorriso, then re-run with --i-ran-smoke"
    fi
    if [ "$attest_vbox" != 1 ]; then
      die "stable needs a VirtualBox boot (TESTING.md step 3), which no tool here verifies.
     do it, then re-run with --i-ran-vbox"
    fi
  fi

  local orig pre
  orig="$(git -C "$PRIMARY" symbolic-ref --short HEAD 2>/dev/null || echo "$target")"
  git -C "$PRIMARY" switch "$target" >/dev/null 2>&1 || die "cannot switch to $target"
  pre="$(git -C "$PRIMARY" rev-parse "$target")"
  say "${c_dim}$target at ${pre:0:12} (pre-merge). trial-merging $src ...${c_off}"

  if ! git -C "$PRIMARY" merge --no-ff --no-commit "$src" >/dev/null 2>&1; then
    git -C "$PRIMARY" merge --abort 2>/dev/null
    git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1
    die "merge conflicts with $target; $target left untouched at ${pre:0:12}"
  fi

  if cmd_gate --no-mark "$PRIMARY"; then
    git -C "$PRIMARY" commit --no-edit \
      -m "Merge $src into $target (gate: $(gate_kind) PASS)" >/dev/null
    local new; new="$(git -C "$PRIMARY" rev-parse "$target")"
    git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1
    ok "merged $src into $target -> ${new:0:12}  (NOT pushed)"
    if [ "$target" = stable ]; then
      say ""; say "add a row to docs/BRANCHING.md merge log (newest first):"
      say "   $(date -u +%F)  ${new:0:7}     <what this proves works>"
    fi
    say ""; say "roll back this merge (local, unpushed):"
    say "   tools/ws.sh rollback $target --to-sha $pre"
    return 0
  else
    git -C "$PRIMARY" merge --abort 2>/dev/null
    git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1
    die "gate FAILED on the merged tree; $target left untouched at ${pre:0:12}"
  fi
}

# ---------------------------------------------------------------------------
# rollback: undo a merge. reset for unpushed, revert for pushed.
# ---------------------------------------------------------------------------
cmd_rollback() {
  local target="${1:-}"; shift || true
  case "$target" in main|stable) ;; *) die "usage: ws rollback <main|stable> --to-sha <sha> | --revert <merge-sha>";; esac
  local mode="" arg=""
  case "${1:-}" in
    --to-sha) mode=reset; arg="${2:-}";;
    --revert) mode=revert; arg="${2:-}";;
    *) die "rollback: need --to-sha <sha> (unpushed) or --revert <merge-sha> (pushed)";;
  esac
  [ -n "$arg" ] || die "rollback: missing sha"
  local here; here="$(git rev-parse --show-toplevel 2>/dev/null)"
  [ "$here" = "$PRIMARY" ] || die "run rollback from the main checkout: $PRIMARY"
  [ -z "$(git -C "$PRIMARY" status --porcelain)" ] || die "main checkout is dirty"

  local orig; orig="$(git -C "$PRIMARY" symbolic-ref --short HEAD)"
  git -C "$PRIMARY" switch "$target" >/dev/null 2>&1 || die "cannot switch to $target"
  if [ "$mode" = reset ]; then
    git -C "$PRIMARY" merge-base --is-ancestor "$arg" "$target" \
      || { git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1; die "$arg is not an ancestor of $target; refusing (would lose commits)"; }
    git -C "$PRIMARY" reset --hard "$arg" >/dev/null
    ok "$target reset to ${arg:0:12} (local)"
  else
    git -C "$PRIMARY" revert -m 1 --no-edit "$arg" >/dev/null \
      || { git -C "$PRIMARY" revert --abort 2>/dev/null; git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1; die "revert failed"; }
    ok "$target: reverted merge ${arg:0:12} as a new commit"
  fi
  git -C "$PRIMARY" switch "$orig" >/dev/null 2>&1
}

cmd_drop() {
  local name="${1:-}"; [ -n "$name" ] || die "usage: ws drop <name>"
  local path="$WT_ROOT/$name" br="wt/$name"
  [ -d "$path" ] && git -C "$PRIMARY" worktree remove "$path" --force >/dev/null 2>&1 && ok "removed worktree ${path/#$PRIMARY\//}"
  git -C "$PRIMARY" branch -D "$br" >/dev/null 2>&1 && ok "deleted branch $br" || warn "branch $br not deleted (unmerged? use git branch -D $br)"
}

cmd_doctor() {
  say "ws doctor - $(date -u +%FT%TZ)"
  say "repo: $PRIMARY"
  for t in bash git timeout cargo rustup; do have "$t" && ok "$t" || fail "$t MISSING (required)"; done
  local kind; kind="$(gate_kind)"
  if [ "$kind" = smoke ]; then
    ok "gate = smoke (qemu + xorriso present): builds + boots QEMU, steps 1-2 automated"
  else
    ok "gate = build (cargo $(cargo_tc) build --release), zero-warning bar"
    have qemu-system-x86_64 || warn "qemu-system-x86_64 MISSING - QEMU smoke (TESTING.md step 2) cannot run here"
    have xorriso || warn "xorriso MISSING - cannot stage the ISO here"
    say "   steps 2-3 (QEMU smoke, VirtualBox) must run on your Linux/WSL/VM box;"
    say "   merges to stable require --i-ran-smoke --i-ran-vbox here."
  fi
  rustup toolchain list 2>/dev/null | grep -q '^nightly' && ok "nightly toolchain present" \
    || fail "nightly MISSING - rose needs it (limine crate); set ROSE_CARGO_TOOLCHAIN or install nightly"
}

cmd_help() { sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

main() {
  local sub="${1:-help}"; shift || true
  case "$sub" in
    doctor)   cmd_doctor "$@";;
    new)      cmd_new "$@";;
    gate)     cmd_gate "$@";;
    check)    cmd_check "$@";;
    merge)    cmd_merge "$@";;
    rollback) cmd_rollback "$@";;
    drop)     cmd_drop "$@";;
    help|-h|--help) cmd_help;;
    *) die "unknown command '$sub' (try: ws help)";;
  esac
}
main "$@"
