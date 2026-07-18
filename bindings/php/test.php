<?php
// straitjacket-allow-file[:duplication] — this tiny PHP check() driver mirrors
// the throwaway/php-hello harness on purpose; both are minimal standalone test
// loops, and that overlap is expected rather than something to refactor away.
//
// Test harness for the atilla-php extension (M0 scaffold).
//
// Loaded by test.sh via `php -d extension=<abs-path>/libatilla_php.so`, with the
// real atilla-core version passed in the ATILLA_EXPECTED_VERSION env var so the
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

// The extension name PHP registers is the crate package name, "atilla-php"
// (hyphenated); the .so file is libatilla_php.so (underscored [lib] name).
if (!extension_loaded('atilla-php')) {
    fwrite(STDERR, "ERROR: extension 'atilla-php' is not loaded\n");
    exit(2);
}

if (!class_exists('Atilla')) {
    fwrite(STDERR, "ERROR: class 'Atilla' is not registered\n");
    exit(2);
}

$expected = getenv('ATILLA_EXPECTED_VERSION');
if ($expected === false || $expected === '') {
    fwrite(STDERR, "ERROR: ATILLA_EXPECTED_VERSION not set\n");
    exit(2);
}

$version = Atilla::version();

check('Atilla::version is a string', is_string($version), true);
check('Atilla::version is non-empty', $version !== '', true);
check('Atilla::version matches atilla-core', $version, $expected);

if ($failures === 0) {
    echo "\nALL TESTS PASSED\n";
    exit(0);
}
fwrite(STDERR, "\n$failures test(s) failed\n");
exit(1);
