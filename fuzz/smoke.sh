#!/usr/bin/env bash
# The "Fuzz smoke run" check: builds every fuzz target once, then runs each
# for a minute (or the seconds given), one per CPU at a time, so a 4-CPU
# runner gets through all of them in a quarter of the time. Each target
# still gets its full minute on a CPU of its own. A target with a
# dictionary in fuzz/dicts/<target>.dict uses it.
#
#   FUZZ_TOOLCHAIN=nightly-2026-10-31 fuzz/smoke.sh [seconds]
#
# Prints every target's libFuzzer output in a group, and fails if any
# target crashed; the input that crashed it is in fuzz/artifacts/<target>/.

set -u
secs=${1:-60}
toolchain=${FUZZ_TOOLCHAIN:-nightly}
# The prebuilt cargo-fuzz is a musl binary and would default to the musl
# target, which AddressSanitizer does not support.
triple=x86_64-unknown-linux-gnu
cd "$(dirname "$0")/.."

cargo "+$toolchain" fuzz build --target "$triple" || exit 1
targets=$(cargo "+$toolchain" fuzz list) || exit 1
logs=$(mktemp -d)

run_one() {
    local t=$1 dict=()
    if [ -f "fuzz/dicts/$t.dict" ]; then dict=("-dict=fuzz/dicts/$t.dict"); fi
    # The build above is up to date, so cargo fuzz run only starts the target.
    cargo "+$toolchain" fuzz run --target "$triple" "$t" -- \
        -max_total_time="$secs" "${dict[@]}" >"$logs/$t.log" 2>&1
    echo $? >"$logs/$t.status"
}

cpus=$(nproc)
for t in $targets; do
    while [ "$(jobs -rp | wc -l)" -ge "$cpus" ]; do wait -n; done
    run_one "$t" &
done
wait

failed=0
for t in $targets; do
    status=$(cat "$logs/$t.status" 2>/dev/null || echo "?")
    echo "::group::$t (exit $status)"
    cat "$logs/$t.log"
    echo "::endgroup::"
    if [ "$status" != 0 ]; then
        echo "::error::fuzz target $t failed (exit $status); its input is in fuzz/artifacts/$t/"
        failed=1
    fi
done
exit $failed
