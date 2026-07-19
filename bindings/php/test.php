<?php
// straitjacket-allow-file[:duplication] — this tiny PHP check() driver mirrors
// the throwaway/php-hello harness on purpose; both are minimal standalone test
// loops, and that overlap is expected rather than something to refactor away.
//
// Test harness for the pidgin-php extension (M0 scaffold).
//
// Loaded by test.sh via `php -d extension=<abs-path>/libpidgin_php.so`, with the
// real pidgin-core version passed in the PIDGIN_EXPECTED_VERSION env var so the
// assertion checks the extension against the actual workspace version rather
// than a value duplicated here.

$failures = 0;

function check(string $label, $got, $expected): void {
    global $failures;
    if ($got === $expected) {
        echo "PASS  $label => " . var_export($got, true) . "\n";
    } else {
        echo "FAIL  $label => got " . var_export($got, true)
            . ", expected " . var_export($expected, true) . "\n";
        $failures++;
    }
}

// The extension name PHP registers is the crate package name, "pidgin-php"
// (hyphenated); the .so file is libpidgin_php.so (underscored [lib] name).
if (!extension_loaded('pidgin-php')) {
    fwrite(STDERR, "ERROR: extension 'pidgin-php' is not loaded\n");
    exit(2);
}

if (!class_exists('Pidgin')) {
    fwrite(STDERR, "ERROR: class 'Pidgin' is not registered\n");
    exit(2);
}

$expected = getenv('PIDGIN_EXPECTED_VERSION');
if ($expected === false || $expected === '') {
    fwrite(STDERR, "ERROR: PIDGIN_EXPECTED_VERSION not set\n");
    exit(2);
}

$version = Pidgin::version();

check('Pidgin::version is a string', is_string($version), true);
check('Pidgin::version is non-empty', $version !== '', true);
check('Pidgin::version matches pidgin-core', $version, $expected);

if ($failures === 0) {
    echo "\nALL TESTS PASSED\n";
    exit(0);
}
fwrite(STDERR, "\n$failures test(s) failed\n");
exit(1);
