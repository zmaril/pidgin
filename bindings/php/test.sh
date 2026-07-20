#!/usr/bin/env bash
# Build the pidgin-php extension and prove it loads in PHP and returns the real
# pidgin-core version.
#
# Fails loudly if anything is off: the cdylib does not build, PHP cannot load
# the .so, the version does not match the workspace version, or the test script
# exits non-zero.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

# cargo names the cdylib libpidgin_php.so on Linux and libpidgin_php.dylib on
# macOS. dlopen does not care about the suffix, so accept either.
case "$(uname -s)" in
    Darwin) libext="dylib" ;;
    *)      libext="so" ;;
esac

profile="${1:-debug}"
if [[ "$profile" == "release" ]]; then
    cargo build --release
    so="$here/target/release/libpidgin_php.$libext"
else
    cargo build
    so="$here/target/debug/libpidgin_php.$libext"
fi

if [[ ! -f "$so" ]]; then
    echo "ERROR: expected extension not found at $so" >&2
    exit 1
fi

# The authoritative version comes from the pidgin-core façade, obtained the same
# way the binding does (the crate's own CARGO_PKG_VERSION = the workspace
# version). We read it straight from the façade's Cargo metadata so the test has
# an independent source of truth to compare the extension's answer against.
expected="$(
    cargo metadata --format-version 1 --no-deps \
        --manifest-path "$here/../../crates/pidgin-core/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])'
)"

if [[ -z "$expected" ]]; then
    echo "ERROR: could not determine pidgin-core version" >&2
    exit 1
fi

echo "pidgin-core version (expected): $expected"
echo "loading extension: $so"
echo

PIDGIN_EXPECTED_VERSION="$expected" php -d extension="$so" "$here/test.php"
