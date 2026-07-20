#!/usr/bin/env bash
# serve.sh — launch the pidgin PHP chat demo on the PHP built-in server with the
# pidgin-php extension loaded.
#
# Resolves the built .so (prefers target/release, else target/debug, both under
# bindings/php), then runs `php -d extension=<so> -S 127.0.0.1:8080 -t demo/`.
# No php.ini edit required. Mode (FAUX/LIVE) follows ANTHROPIC_API_KEY.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # bindings/php/demo
php_dir="$(cd "$here/.." && pwd)"                        # bindings/php

release_so="$php_dir/target/release/libpidgin_php.so"
debug_so="$php_dir/target/debug/libpidgin_php.so"

if [[ -f "$release_so" ]]; then
    so="$release_so"
elif [[ -f "$debug_so" ]]; then
    so="$debug_so"
else
    echo "ERROR: pidgin-php extension not built." >&2
    echo "       Expected one of:" >&2
    echo "         $release_so" >&2
    echo "         $debug_so" >&2
    echo "       Build it first:  (cd \"$php_dir\" && cargo build)" >&2
    echo "       For the live path:  cargo build --features native-http" >&2
    exit 1
fi

host="127.0.0.1"
port="${PORT:-8080}"

if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
    mode="LIVE (real Anthropic API via native transport)"
else
    mode="FAUX (offline, deterministic echoes; no API key set)"
fi

echo "pidgin PHP demo"
echo "  extension: $so"
echo "  mode:      $mode"
echo "  open:      http://$host:$port"
echo

exec php -d extension="$so" -S "$host:$port" -t "$here"
