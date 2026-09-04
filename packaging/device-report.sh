#!/usr/bin/env bash
#
# Say what the device tests could actually reach on this machine.
#
# Ten of this shell's tests need hardware: `/dev/uinput`, a controller the
# kernel drives, or the second-generation Steam Controller. On a machine
# without them each one prints a line and passes, which is right — a machine
# with no controller has nothing to say about controllers, and a suite that
# failed there would be a suite nobody could run.
#
# What was wrong is that the line went nowhere. `cargo test` counts a skipped
# test as a pass, so a release built on a machine with no pad reported full
# coverage of the controller work and had none of it. This runs those tests on
# their own and prints what was covered and what was not, by name, so that a
# release note can say which it was.
#
# It fails only on a real failure. A skip is reported, never fatal: it is a
# fact about the machine this was run on.
#
# Usage:
#   packaging/device-report.sh

set -euo pipefail

packaging_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$packaging_dir/lib.sh"

require_command cargo

# The three modules that touch a device, and nothing else: this is a report
# about hardware, and a run of the whole suite would bury it.
modules=(pad_guard:: steam_hid:: steam_stand_in::)

package_note "running the device tests"
output="$(mktemp)"
trap 'rm -f "$output"' EXIT

# `--nocapture` because the skip lines are the point of the run, and the
# harness swallows the output of a test that passes. Threads left alone, so
# each test still names its own thread — which is where `crate::skipped` reads
# the test's name from.
status=0
(cd "$PROJECT_ROOT" && cargo test -p lxb-desktop --bins -- "${modules[@]}" --nocapture) \
    > "$output" 2>&1 || status=$?

ran="$(grep -cE '^test .* \.\.\. ok$' "$output" || true)"
skipped="$(grep -c '^skipped: ' "$output" || true)"

if ((status != 0)); then
    cat "$output" >&2
    package_die "the device tests failed"
fi

covered=$((ran - skipped))
package_note "device tests: $covered covered, $skipped skipped for want of hardware"
if ((skipped > 0)); then
    # Verbatim, including the reason each test gave. A summary that said only
    # "some were skipped" would be the same silence in a longer sentence.
    sed -n 's/^skipped: /    not covered here — /p' "$output"
    echo "    A release built here has no coverage of those. Say so, or run it"
    echo "    again on a machine with the hardware."
fi
