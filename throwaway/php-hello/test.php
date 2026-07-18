<?php
// Test harness for the php-hello Rust extension.
// Run with:  php -d extension=$(pwd)/target/debug/libphp_hello.so test.php

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

// Sanity: the extension must actually be loaded.
// ext-php-rs names the extension after the crate package ("php-hello"),
// while the .so file is libphp_hello.so (from the [lib] name).
if (!extension_loaded('php-hello')) {
    fwrite(STDERR, "ERROR: extension 'php-hello' is not loaded\n");
    exit(2);
}

check('pi_hello', pi_hello('Zack'), 'Hello, Zack, from Rust!');
check('pi_add',   pi_add(40, 2),    42);

$g = new PiGreeter('spike');
check('PiGreeter::greet', $g->greet('world'), 'spike: hello world (from Rust)');

if ($failures === 0) {
    echo "\nALL TESTS PASSED\n";
    exit(0);
}
fwrite(STDERR, "\n$failures test(s) failed\n");
exit(1);
