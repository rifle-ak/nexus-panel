#!/usr/bin/env php
<?php
/**
 * Runs every test_* function in tests/*Test.php. No framework needed: the
 * module has to work on a stock WHMCS host, and its tests should run on a
 * stock PHP.
 *
 *   php whmcs/tests/run.php
 */

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

$files = glob(__DIR__ . '/*Test.php') ?: [];
sort($files);
foreach ($files as $file) {
    require_once $file;
}

$tests = array_values(array_filter(get_defined_functions()['user'], static fn (string $f): bool => strpos($f, 'test_') === 0));
sort($tests);

$passed = 0;
$failed = [];
foreach ($tests as $test) {
    // Fresh fake WHMCS state per test.
    $GLOBALS['nexuspanel_test_log'] = [];
    unset($GLOBALS['nexuspanel_client_factory']);
    try {
        $test();
        $passed++;
        echo "ok   $test\n";
    } catch (\Throwable $e) {
        $failed[] = $test;
        echo "FAIL $test\n     " . get_class($e) . ': ' . $e->getMessage() . "\n";
        if (!$e instanceof TestFailure) {
            echo '     at ' . $e->getFile() . ':' . $e->getLine() . "\n";
        }
    }
}

echo "\n$passed passed, " . count($failed) . " failed\n";
exit($failed === [] ? 0 : 1);
