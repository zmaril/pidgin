#!/usr/bin/env bash
# Run `quint verify` on the GOOD bridge spec with a one-shot retry, so a flaky
# Apalache download / JVM warmup in CI does not spuriously fail the job. A
# passing spec produces no counterexample, so a retry can only recover from
# infrastructure flakiness — it can never turn a real invariant violation green.
#
# Usage: scripts/quint-verify.sh <invariant> <spec.qnt>
set -euo pipefail

inv="${1:?invariant name required}"
spec="${2:?spec path required}"
max_steps="${QUINT_MAX_STEPS:-5}"

for attempt in 1 2; do
  echo "::group::quint verify --invariant ${inv} ${spec} (attempt ${attempt})"
  if quint verify --invariant "${inv}" --max-steps "${max_steps}" "${spec}"; then
    echo "::endgroup::"
    echo "OK: ${inv} holds on ${spec}"
    exit 0
  fi
  echo "::endgroup::"
  echo "quint verify attempt ${attempt} for ${inv} failed; retrying after warmup..." >&2
  sleep 5
done

echo "FAILED: quint verify --invariant ${inv} ${spec} did not pass" >&2
exit 1
