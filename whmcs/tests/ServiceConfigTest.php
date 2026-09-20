<?php

declare(strict_types=1);

use WHMCS\Module\Server\NexusPanel\ServiceConfig;

function test_product_settings_map_to_the_create_payload(): void
{
    $params = testParams();
    $payload = ServiceConfig::fromParams($params)->toCreatePayload($params);

    assertSame('whmcs-123', $payload['external_id']);
    assertSame("Ryan's Minecraft (Paper) server", $payload['name']);
    assertSame('minecraft-paper', $payload['blueprint']);
    assertSame(4096, $payload['memory_mb']);
    assertSame(2000, $payload['cpu_millicores']);
    assertSame(20480, $payload['disk_mb']);
    assertSame(true, $payload['auto_start']);
    assertSame('client-42 <ryan@example.test>', $payload['owner']);
    assertSame(
        ['VIEW_DISTANCE' => '8', 'SERVER_NAME' => 'Example Host', 'MAX_PLAYERS' => '20'],
        $payload['variables']
    );
}

function test_defaults_when_the_product_is_barely_configured(): void
{
    $params = testParams([
        'configoption1' => '', 'configoption2' => '', 'configoption3' => 'abc', 'configoption4' => '-5',
        'configoption5' => '0', 'configoption6' => '', 'configoption8' => '',
        'clientsdetails' => [],
    ]);
    unset($params['configoption7']);
    $c = ServiceConfig::fromParams($params);
    assertSame('minecraft-paper', $c->blueprint);
    assertSame(ServiceConfig::DEFAULT_MEMORY_MB, $c->memoryMb);
    assertSame(ServiceConfig::DEFAULT_CPU_MILLICORES, $c->cpuMillicores);
    assertSame(ServiceConfig::DEFAULT_DISK_MB, $c->diskMb);
    assertSame(ServiceConfig::DEFAULT_SLOTS, $c->slots);
    assertSame(true, $c->autoStart, 'auto start defaults on');
    assertSame(ServiceConfig::DEFAULT_SESSION_HOURS * 3600, $c->sessionTtlSecs());
    assertSame('Minecraft (Paper) server #123', $c->name);
}

function test_configurable_options_override_the_product(): void
{
    $params = testParams([
        'configoptions' => [
            'Memory (GB)' => '8',
            'CPU Cores' => '3',
            'Storage (GB)' => '50',
            'Player Slots' => '64',
            'Game' => 'rust',
        ],
    ]);
    $c = ServiceConfig::fromParams($params);
    assertSame(8192, $c->memoryMb);
    assertSame(3000, $c->cpuMillicores);
    assertSame(51200, $c->diskMb);
    assertSame(64, $c->slots);
    assertSame('rust', $c->blueprint);
    assertSame("Ryan's Rust (Oxide) server", $c->name);

    // MB-unit variants and empty values.
    $c = ServiceConfig::fromParams(testParams(['configoptions' => ['RAM' => '2048MB', 'Disk' => '', 'Memory' => 'lots']]));
    assertSame(2048, $c->memoryMb, 'a later non-numeric value does not clobber');
    assertSame(20480, $c->diskMb);
}

function test_custom_fields_set_the_name_and_variables(): void
{
    $params = testParams([
        'customfields' => [
            'Server Name' => 'Bob\'s Deathmatch',
            'SERVER_PASSWORD' => 'hunter2',
            'Something Else' => 'ignored',
        ],
    ]);
    $c = ServiceConfig::fromParams($params);
    assertSame("Bob's Deathmatch", $c->name);
    assertSame('hunter2', $c->variables['SERVER_PASSWORD']);
    assertTrue(!isset($c->variables['Something Else']));
}

function test_domain_is_the_name_when_set(): void
{
    $c = ServiceConfig::fromParams(testParams(['domain' => 'play.example.test']));
    assertSame('play.example.test', $c->name);
}

function test_variables_parse_loosely_but_safely(): void
{
    $vars = ServiceConfig::parseVariables("A=1\r\n  B = two words \n#C=3\nnot a var\n=nokey\n9BAD=x\nD=a=b");
    assertSame(['A' => '1', 'B' => 'two words', 'D' => 'a=b'], $vars);
}

function test_explicit_max_players_wins_over_slots(): void
{
    $c = ServiceConfig::fromParams(testParams(['configoption6' => 'MAX_PLAYERS=99']));
    assertSame('99', $c->allVariables()['MAX_PLAYERS']);
}

function test_package_payload_has_no_name_or_ports(): void
{
    $p = ServiceConfig::fromParams(testParams())->toPackagePayload();
    assertSame(['memory_mb', 'cpu_millicores', 'disk_mb', 'variables'], array_keys($p));
}
