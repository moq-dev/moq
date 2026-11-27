#!/usr/bin/env bash
# Run kio's thread-spawning unit tests under Miri across many scheduler seeds:
# sh/rs/miri-seeds.sh [SEEDS]. Random schedules, not exhaustive: a pass is evidence,
# not proof. Nightly only, as sh/rs/miri.sh is.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

COMPONENTS=miri,rust-src source sh/rs/nightly.sh
seeds=${1:-8}

# Every kio library test that spawns a thread or a Tokio worker (grep `rs/kio/src/` for
# `thread::` and `new_multi_thread`). A rename fails the count below, not silently.
names=(
    channel::tests::racing_publishes_and_closes_wake_every_waiter
    clock::captured::tests::a_paused_runtime_reads_the_same_everywhere
    clock::deadline::tests::entering_nests
    clock::installed::tests::advance_wakes_a_parked_timer
    clock::installed::tests::an_entered_queue_keeps_the_installed_clock
    clock::random::tests::the_host_seed_holds_while_draws_move
    clock::runtime::tests::threads_share_one_origin
    clock::sim::tests::the_probe_fails_a_wake_from_another_thread
    clock::sim::tests::the_probe_fails_a_wake_from_another_thread_as_the_root_finishes
    task::tests::a_wake_during_thread_teardown_reaches_the_owner
    task::tests::concurrent_wakes_are_never_lost
    tests::consumer_creation_with_existing_demand_does_not_relock
    weak::test::upgrade_never_wins_a_closing_channel
)

out=$(mktemp)
trap 'rm -f "$out"' EXIT
run() {
    local count=$1
    shift
    "$@" 2>&1 | tee "$out"
    # Seeds run in parallel, so their result lines can interleave: match loosely.
    grep -Eq "ok\. $count passed" "$out" || {
        echo "rs: expected $count tests per seed: $*" >&2
        exit 1
    }
}

echo "seeds 0..$seeds"
# Stacked Borrows with leak checking.
MIRIFLAGS="-Zmiri-many-seeds=0..$seeds" run "${#names[@]}" \
    cargo miri test --locked -p kio --lib -- --exact "${names[@]}"
# The log on Tree Borrows, as sh/rs/miri.sh runs it.
MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-ignore-leaks -Zmiri-many-seeds=0..$seeds" run 1 \
    cargo miri test --locked -p kio --lib -- --exact log::test::concurrent_readers_see_every_entry
