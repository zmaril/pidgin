#!/usr/bin/env bash
# Build the atilla-php extension and prove it loads in PHP and returns the real
# atilla-core version.
#
# Fails loudly if anything is off: the cdylib does not build, PHP cannot load
# the .so, the version does not match the workspace version, or the test script
# exits non-zero.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

profile="${1:-debug}"
if [[ "$profile" == "release" ]]; then
    cargo build --release
    so="$here/target/release/libatilla_php.so"
else
    cargo build
    so="$here/target/debug/libatilla_php.so"
fi

if [[ ! -f "$so" ]]; then
    echo "ERROR: expected extension not found at $so" >&2
    exit 1
fi

# The authoritative version comes from the atilla-core façade, obtained the same
# way the binding does (the crate's own CARGO_PKG_VERSION = the workspace
# version). We read it straight from the façade's Cargo metadata so the test has
# an independent source of truth to compare the extension's answer against.
expected="$(
    cargo metadata --format-version 1 --no-deps \
        --manifest-path "$here/../../crates/atilla-core/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])'
)"

if [[ -z "$expected" ]]; then
    echo "ERROR: could not determine atilla-core version" >&2
    exit 1
fi

echo "atilla-core version (expected): $expected"
echo "loading extension: $so"
echo

ATILLA_EXPECTED_VERSION="$expected" php -d extension="$so" "$here/test.php"
