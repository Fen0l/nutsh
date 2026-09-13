#!/usr/bin/env bash
# Read-only acceptance pass against a real Prism Central. Everything else is proved against
# crates/mockpc; this is the half that needs a live machine and a person to launch it.
#
# SAFETY: every nutsh process presents the password exactly once, because the session cookie is
# per process. A loop of processes against a BAD password is therefore a loop of authentication
# failures, which is how an account with a lockout policy gets locked out. So: the first sign of
# a refused credential aborts the whole run, and nothing below ever retries. stdin is /dev/null
# everywhere, so a missing stored secret fails fast instead of prompting once per iteration.
#
# Usage:  bash acceptance.sh [phase ...]
# Phases: preflight (no network) | check | rates | details | sweep | all
# Default: preflight check rates      (the cheap, high-value half: 7 authentications)

set -uo pipefail

REPO=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
CTX=${CTX:?set CTX to the nutsh context to run against}
# Never inside the repo: these are run transcripts, not source.
OUT=${OUT:-${TMPDIR:-/tmp}/nutsh-lab-$(date +%Y%m%d-%H%M%S)}
LOGFILE="${XDG_STATE_HOME:-$HOME/.local/state}/nutsh/logs/nutsh.log"
# The host-wide bound this client will not widen past, whatever a header says
# (`bucket::HOST_CEILING`). It is NOT the number to judge a single endpoint by: a real Prism
# Central advertises a different tier per endpoint -- pc.7.6 states ten of them, from 2 to 30 --
# so a host total of twelve a second can be every endpoint inside its own limit. The
# per-endpoint comparison below is the one that means anything.
HOST_CEILING=30

cd "$REPO" || exit 1
mkdir -p "$OUT"
AUTHS=0

say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }

abort() {
  printf '\n\033[1;31m================ ABORTED ================\033[0m\n' >&2
  printf '\033[1;31m%s\033[0m\n' "$*" >&2
  cat >&2 <<'MSG'

Nothing further will run. The credential was not retried.
Before running this again, confirm by hand that the account is not locked and that the
stored password is current. Do NOT simply re-launch.
MSG
  echo "$*" > "$OUT/ABORTED"
  exit 9
}

# The valve. Runs nutsh once, never twice. Aborts the whole script on anything that smells
# like a refused credential. Exit 2 is expected here (18/20 namespaces), exit 3 never is.
run_nutsh() {
  local label=$1; shift
  local f="$OUT/$label.out"
  AUTHS=$((AUTHS + 1))
  ./target/debug/nutsh "$@" >"$f" 2>"$OUT/$label.err" </dev/null
  local rc=$?
  if grep -qiE '(^|[^0-9])401([^0-9]|$)|unauthoriz|authentication failed|invalid credential|locked|too many' "$OUT/$label.err" "$f" 2>/dev/null; then
    abort "[$label] the Prism Central refused the credential (or the account is locked). rc=$rc
$(head -5 "$OUT/$label.err")"
  fi
  if [ $rc -eq 3 ] && grep -qE 'namespace not served|needs [a-z]+ v[0-9.]+ \(this PC pinned' "$OUT/$label.err"; then
    # The exit-3s this script can classify: the kind's namespace is not deployed on this Prism
    # Central (opsmgmt here), or the kind needs a spec version the PC does not pin (multidomain
    # v4.3 against a v4.2 PC). No credential was involved in either refusal.
    note "$label: skipped, $(grep -oE 'namespace not served|needs [a-z]+ v[0-9.]+ \(this PC pinned v[0-9.]+\)' "$OUT/$label.err" | head -1)"
    echo "$rc" > "$OUT/$label.rc"
    return $rc
  fi
  if [ $rc -eq 3 ]; then
    abort "[$label] nutsh exited 3 (error). Treated as fatal on purpose -- an error this script
cannot classify must not be followed by another login attempt.
$(head -5 "$OUT/$label.err")"
  fi
  echo "$rc" > "$OUT/$label.rc"
  return $rc
}

# Move the log aside so the next run's request lines stand alone, then measure that run.
log_reset() { [ -f "$LOGFILE" ] && mv -f "$LOGFILE" "$OUT/discarded-$(date +%s).log"; return 0; }
log_take()  { [ -f "$LOGFILE" ] && cp -f "$LOGFILE" "$OUT/$1.log"; return 0; }

measure() {  # measure <label> -- request rates out of a captured debug log
  python3 - "$OUT/$1.log" "$1" "$HOST_CEILING" <<'ANALYSE'
import sys, re, datetime, pathlib
from collections import defaultdict

path, label, ceiling = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])
if not path.exists():
    print(f"  {label}: no log captured"); raise SystemExit

TS = r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z"
REQ = re.compile(TS + r".*?: request method=(\S+) url=(\S+) status=(\d+)")
# The tier rides on a response, so it belongs to the endpoint of the request just before it.
ADV = re.compile(TS + r".*?(?:adopting the advertised rate limit"
                      r"|advertises a tighter rate limit; slowing to it) count=(\d+) per_secs=(\d+)")

def endpoint(url):
    tail = url.split("?", 1)[0].split("/api/", 1)[-1]
    return re.sub(r"/[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,}", "/{id}", tail)

def peak(ts):
    """Most requests in ANY rolling one-second window. Not the mean, and not a fixed window: a
    fixed one once let five out in a second on a tier of three by rolling at the seam."""
    ts = sorted(ts); worst = j = 0
    for i, _ in enumerate(ts):
        while ts[i] - ts[j] >= 1.0:
            j += 1
        worst = max(worst, i - j + 1)
    return worst

reqs, tiers, last = [], {}, None
for line in path.read_text(errors="replace").splitlines():
    m = REQ.search(line)
    if m:
        last = endpoint(m.group(3))
        reqs.append((datetime.datetime.fromisoformat(m.group(1)).timestamp(), last, int(m.group(4))))
        continue
    a = ADV.search(line)
    if a and last:
        tiers[last] = min(tiers.get(last, 10**9), int(a.group(2)))

if not reqs:
    print(f"  {label}: 0 request lines (is NUTSH_LOG=debug reaching the file?)"); raise SystemExit

span = reqs[-1][0] - reqs[0][0]
host = peak([t for t, _, _ in reqs])
by_ep = defaultdict(list)
for t, ep, _ in reqs:
    by_ep[ep].append(t)
over = [(ep, peak(ts), tiers[ep]) for ep, ts in by_ep.items()
        if ep in tiers and peak(ts) > tiers[ep]]
codes = defaultdict(int)
for _, _, s in reqs:
    codes[s] += 1

verdict = "OK" if host <= ceiling else f"*** OVER THE HOST CEILING OF {ceiling}/s ***"
print(f"  {label}: {len(reqs)} requests over {span:.2f}s = "
      f"{len(reqs)/span if span else 0:.2f} req/s mean, "
      f"host peak {host} in any rolling 1s  [{verdict}]")
print(f"      statuses: {dict(sorted(codes.items()))}"
      + ("   <- a 429 is the Prism Central pushing back" if 429 in codes else ""))
if over:
    for ep, pk, tier in sorted(over, key=lambda o: -o[1]):
        print(f"      *** {ep} reached {pk}/s against its advertised {tier}/s")
else:
    print(f"      every endpoint inside its own advertised tier "
          f"({len(tiers)} of {len(by_ep)} stated one)")
ANALYSE
}

phase_preflight() {
  say "PHASE preflight -- nothing here touches the network"
  local bad=0
  # Build rather than guess from mtimes: `cargo xtask gen-catalog` touches generated.rs without
  # changing a byte of it, so "source newer than binary" reports a rebuild that is not needed.
  # The whole point is exercising the five fixes, so the binary must come from THIS tree.
  export PATH="$HOME/.cargo/bin:$PATH"
  note "cargo build (incremental; no release artefact is produced -- that stays unapproved)"
  if cargo build --workspace --bin nutsh >"$OUT/build.log" 2>&1; then
    note "binary is current with the tree: $(git rev-parse --short HEAD)"
  else
    note "BUILD FAILED -- see $OUT/build.log"; tail -15 "$OUT/build.log"; bad=1
  fi
  grep -q "contexts.$CTX" "${XDG_CONFIG_HOME:-$HOME/.config}/nutsh/config.toml" 2>/dev/null \
    && note "context $CTX present" || { note "MISSING context $CTX"; bad=1; }
  local sec="${XDG_STATE_HOME:-$HOME/.local/state}/nutsh/secrets"
  if ls "$sec" 2>/dev/null | grep -q .; then
    note "stored secret present: $(ls "$sec" | tr '\n' ' ')"
    note "  -> ctx add / ctx login are ALREADY DONE. Do not run ctx login again;"
    note "     it would spend an authentication for nothing."
  else
    note "NO stored secret -- every run below would prompt, and stdin is /dev/null."
    note "  Run ONCE, by hand:  ./target/debug/nutsh ctx login $CTX"
    bad=1
  fi
  note "git HEAD: $(git rev-parse --short HEAD)  tree: $(git status --porcelain | wc -l) modified"
  [ $bad -eq 0 ] && note "preflight OK" || { echo; note "preflight FAILED -- fix the above first"; exit 1; }
}

phase_check() {
  say "PHASE check -- 1 authentication"
  log_reset
  NUTSH_LOG=debug run_nutsh check --check --context "$CTX"
  local rc=$?
  cat "$OUT/check.out"
  note "exit $rc  (2 is expected: opsmgmt 503, tenancy not served)"
  note "EXPECT: 18/20 namespaces available, and 'multidomain v4.2 (catalog v4.3)'"
  grep -qE '18/20 namespaces' "$OUT/check.out" && note "  18/20 -> as expected" || note "  *** not 18/20 -- read the rows above"
  grep -qE 'multidomain +v4\.2 \(catalog v4\.3\)' "$OUT/check.out" && note "  multidomain v4.2 (catalog v4.3) -> as expected" || note "  *** multidomain line differs -- read it above"
  # --check logs to stderr, not the file.
  cp -f "$OUT/check.err" "$OUT/check.log" 2>/dev/null
  measure check
}

phase_rates() {
  say "PHASE rates -- 6 authentications (3 cold, 3 warm)"
  note "cold baseline measured once: VM 0.40 - Dashboard 0.82 - DR 0.47 req/s"
  ./target/debug/nutsh cache clear --context "$CTX" </dev/null >/dev/null 2>&1
  note "cache cleared"
  for v in vm dashboard disaster-recovery; do
    log_reset
    NUTSH_LOG=debug run_nutsh "cold-$v" "$v" --snapshot --readonly --size 120x40 --context "$CTX"
    log_take "cold-$v"; measure "cold-$v"
  done
  # `--snapshot` cannot warm anything: the cache is written on a sixty-second period inside the
  # tick loop (`CACHE_PERIOD`, `app.rs`) and on a context switch, and a snapshot run is done in
  # ten seconds. These rows are a second cold start, worth having as a repeatability check on
  # the cold numbers -- but the warm half of requirement 2 needs a TUI left open past a minute
  # and quit, then a re-run. It is not measurable headlessly.
  say "  second cold start (--snapshot never warms the cache; see the note in this script)"
  for v in vm dashboard disaster-recovery; do
    log_reset
    NUTSH_LOG=debug run_nutsh "warm-$v" "$v" --snapshot --readonly --size 120x40 --context "$CTX"
    log_take "warm-$v"; measure "warm-$v"
  done
  ./target/debug/nutsh cache info --context "$CTX" </dev/null 2>&1 | tee "$OUT/cache-info.txt"
}

phase_details() {
  say "PHASE details -- 9 authentications, the nine snapshotted kinds"
  for k in monitoring.serviceability.Alert clustermgmt.config.Cluster clustermgmt.config.Host \
           datapolicies.config.ProtectionPolicy dataprotection.config.RecoveryPoint \
           clustermgmt.config.StorageContainer networking.config.Subnet prism.config.Task \
           vmm.ahv.config.Vm; do
    run_nutsh "detail-${k##*.}" "$k" --snapshot --readonly --size 140x40 --context "$CTX"
    note "$k -> $OUT/detail-${k##*.}.out"
  done
  note "Read each frame: the detail must compose from the list row and be WHOLE."
  note "A half-empty pane is the narrowing-vs-detail invariant regressing."
}

phase_sweep() {
  say "PHASE sweep -- ~71 authentications, the curated-column sweep"
  local kinds
  kinds=$(python3 - <<'PY'
import re, pathlib
text = pathlib.Path("crates/catalog/nav.toml").read_text()
want = {"Compute & Storage","Network & Security","Data Protection","Hardware","Monitoring","Policies","Admin Center"}
for g in re.split(r"\n\[\[group\]\]\n", text):
    n = re.search(r'name = "([^"]+)"', g)
    if n and n.group(1) in want:
        for kid in re.findall(r'kind = "([^"]+)"', g):
            print(kid)
PY
)
  note "$(echo "$kinds" | wc -l) kinds"
  : > "$OUT/sweep.txt"
  for k in $kinds; do
    # Resume: a kind already run into this OUT is not run again (each run is an authentication).
    [ -f "$OUT/sweep-$k.rc" ] && { note "$k: already run, reusing"; } || \
    run_nutsh "sweep-$k" "$k" --snapshot --readonly --size 140x14 --context "$CTX"
    echo "=== $k" >> "$OUT/sweep.txt"
    sed -n '8,14p' "$OUT/sweep-$k.out" >> "$OUT/sweep.txt"
  done
  note "All frames: $OUT/sweep.txt"
  note "Read every one and check three things:"
  note "  (a) the first six columns are the curated ones, in the curated order"
  note "  (b) no cell is a bare 36-char UUID, and none is an 8-char stub in a column"
  note "      headed for a kind the warm-up covers"
  note "  (c) no column is '-' on every row of a table that HAS rows"
  note "opsmgmt.config.Report greying is CORRECT here: the namespace is not deployed."
}

PHASES=${*:-preflight check rates}
say "nutsh read-only lab acceptance -> $OUT"
note "phases: $PHASES"
[ "$PHASES" = "all" ] && PHASES="preflight check rates details sweep"
for p in $PHASES; do
  case $p in
    preflight|check|rates|details|sweep) "phase_$p" ;;
    *) echo "unknown phase: $p" >&2; exit 1 ;;
  esac
done
say "DONE -- $AUTHS authentications spent. Results: $OUT"
