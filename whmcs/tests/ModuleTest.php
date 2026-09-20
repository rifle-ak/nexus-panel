<?php

declare(strict_types=1);

use WHMCS\Module\Server\NexusPanel\ServiceRecord;

function test_metadata_and_config_options_are_well_formed(): void
{
    $meta = nexuspanel_MetaData();
    assertSame('Nexus Panel', $meta['DisplayName']);
    assertSame(true, $meta['RequiresServer']);

    $options = nexuspanel_ConfigOptions();
    assertSame(8, count($options), 'ServiceConfig reads configoption1..8');
    assertSame('Blueprint', array_key_first($options));
    assertSame('nexuspanel_BlueprintLoader', $options['Blueprint']['Loader']);
    assertTrue(function_exists('nexuspanel_BlueprintLoader'));
}

function test_blueprint_loader_falls_back_when_the_node_is_unreachable(): void
{
    $options = nexuspanel_BlueprintLoader(testParams(['serverhostname' => '', 'serverip' => '']));
    assertTrue(isset($options['minecraft-paper']));

    $node = new FakeNode();
    $node->always('GET', '/api/v1/blueprints', 200, [['id' => 'custom-game', 'name' => 'Custom', 'game' => 'thing']]);
    $node->install();
    $options = nexuspanel_BlueprintLoader(testParams());
    assertSame(['custom-game' => 'Custom (thing)'], $options);
}

function test_test_connection_reports_success_and_failure(): void
{
    $node = new FakeNode();
    $node->on('GET', '/api/v1/node/info', 200, ['node_id' => 'n1', 'version' => '0.1.1']);
    $node->on('GET', '/api/v1/node/info', 401, ['error' => 'Authentication required']);
    $node->install();

    assertSame(['success' => true, 'error' => ''], nexuspanel_TestConnection(testParams()));
    $r = nexuspanel_TestConnection(testParams());
    assertSame(false, $r['success']);
    assertSame('Authentication required', $r['error']);
}

function test_create_account_provisions_and_records_the_server(): void
{
    $node = new FakeNode();
    $node->on('POST', '/api/v1/provision/servers', 201, provisionedServer());
    $node->install();

    $params = testParams();
    assertSame('success', nexuspanel_CreateAccount($params));

    $sent = $node->lastBody();
    assertSame('whmcs-123', $sent['external_id']);
    assertSame('minecraft-paper', $sent['blueprint']);
    assertSame(4096, $sent['memory_mb']);
    assertSame('20', $sent['variables']['MAX_PLAYERS']);

    $props = $params['model']->serviceProperties->values;
    assertSame('srv-abc', $props[ServiceRecord::SERVER_ID]);
    assertSame('203.0.113.10', $props[ServiceRecord::SERVER_IP]);
    assertSame('20000', $props[ServiceRecord::SERVER_PORT]);

    // The API key never reaches the module log in the clear.
    $log = end($GLOBALS['nexuspanel_test_log']);
    assertSame('CreateAccount', $log['action']);
    assertTrue(in_array('node-api-key-secret', $log['replaceVars'], true), 'key masked');
}

function test_create_account_uses_the_whmcs_server_ip_when_the_node_has_none(): void
{
    $node = new FakeNode();
    $node->on('POST', '/api/v1/provision/servers', 201, provisionedServer(['ip' => null]));
    $node->install();
    $params = testParams();
    assertSame('success', nexuspanel_CreateAccount($params));
    assertSame('198.51.100.7', $params['model']->serviceProperties->values[ServiceRecord::SERVER_IP]);
}

function test_create_account_returns_the_nodes_error(): void
{
    $node = new FakeNode();
    $node->on('POST', '/api/v1/provision/servers', 503, ['error' => 'no block of 3 free ports left in 20000-29999']);
    $node->install();
    $params = testParams();
    $result = nexuspanel_CreateAccount($params);
    assertContains('no block of 3 free ports', $result);
    assertTrue(empty($params['model']->serviceProperties->values), 'nothing recorded on failure');
}

function provisionedParams(): array
{
    $params = testParams();
    $params['model']->serviceProperties->save([ServiceRecord::SERVER_ID => 'srv-abc', ServiceRecord::SERVER_IP => '203.0.113.10', ServiceRecord::SERVER_PORT => '20000']);

    return $params;
}

function test_suspend_unsuspend_use_the_recorded_server(): void
{
    $node = new FakeNode();
    $node->always('POST', '/api/v1/containers/srv-abc/suspend', 200, ['ok' => true]);
    $node->always('POST', '/api/v1/containers/srv-abc/unsuspend', 200, ['ok' => true]);
    $node->always('POST', '/api/v1/containers/srv-abc/start', 400, ['error' => 'game files are not installed']);
    $node->install();

    assertSame('success', nexuspanel_SuspendAccount(provisionedParams()));
    // Unsuspend starts the server; a start that cannot happen yet is not a
    // failed unsuspension.
    assertSame('success', nexuspanel_UnsuspendAccount(provisionedParams()));
    $methods = array_map(static fn ($r) => $r['method'] . ' ' . parse_url($r['url'], PHP_URL_PATH), $node->requests);
    assertSame([
        'POST /api/v1/containers/srv-abc/suspend',
        'POST /api/v1/containers/srv-abc/unsuspend',
        'POST /api/v1/containers/srv-abc/start',
    ], $methods);

    assertSame('This service has no server provisioned yet.', nexuspanel_SuspendAccount(testParams()));
}

function test_terminate_removes_the_server_and_forgets_it(): void
{
    $node = new FakeNode();
    $node->always('DELETE', '/api/v1/provision/servers/srv-abc', 200, ['ok' => true, 'existed' => true]);
    $node->install();

    $params = provisionedParams();
    assertSame('success', nexuspanel_TerminateAccount($params));
    assertSame('', $params['model']->serviceProperties->values[ServiceRecord::SERVER_ID]);

    // Terminating again (or a never-provisioned service) is not an error.
    assertSame('success', nexuspanel_TerminateAccount($params));
    assertSame(1, count($node->requests), 'no second call to the node');
}

function test_change_package_sends_new_limits(): void
{
    $node = new FakeNode();
    $node->always('POST', '/api/v1/provision/servers/srv-abc/package', 200, provisionedServer());
    $node->install();

    $params = provisionedParams();
    $params['configoption2'] = '8192';
    $params['configoptions'] = ['Player Slots' => '40'];
    assertSame('success', nexuspanel_ChangePackage($params));
    $sent = $node->lastBody();
    assertSame(8192, $sent['memory_mb']);
    assertSame('40', $sent['variables']['MAX_PLAYERS']);
    assertTrue(!isset($sent['name']), 'name is not part of a package change');
}

function test_custom_buttons_drive_power_actions(): void
{
    $node = new FakeNode();
    $node->always('POST', '/api/v1/containers/srv-abc/start', 200, ['ok' => true]);
    $node->always('POST', '/api/v1/containers/srv-abc/stop', 200, ['ok' => true]);
    $node->always('POST', '/api/v1/containers/srv-abc/restart', 200, ['ok' => true]);
    $node->always('POST', '/api/v1/containers/srv-abc/install', 409, ['error' => 'stop the server before installing its game files']);
    $node->install();

    assertSame('success', nexuspanel_start(provisionedParams()));
    assertSame('success', nexuspanel_stop(provisionedParams()));
    assertSame('success', nexuspanel_restart(provisionedParams()));
    assertSame('stop the server before installing its game files', nexuspanel_reinstall(provisionedParams()));

    foreach (nexuspanel_AdminCustomButtonArray() + nexuspanel_ClientAreaCustomButtonArray() as $fn) {
        assertTrue(function_exists('nexuspanel_' . $fn), "button function nexuspanel_$fn exists");
    }
}

function test_single_sign_on_redirects_to_a_one_time_link(): void
{
    $node = new FakeNode();
    $node->always('POST', '/api/v1/provision/sso', 200, ['token' => 'abc123', 'path' => '/sso/abc123', 'expires_in_secs' => 60]);
    $node->install();

    $r = nexuspanel_ServiceSingleSignOn(provisionedParams());
    assertSame(true, $r['success']);
    assertSame('https://node1.example.test/sso/abc123', $r['redirectTo']);
    $sent = $node->lastBody();
    assertSame('srv-abc', $sent['server_id']);
    assertSame('client-42 <ryan@example.test>', $sent['subject']);
    assertSame(8 * 3600, $sent['session_ttl_secs']);

    // The token is masked in the module log.
    $log = end($GLOBALS['nexuspanel_test_log']);
    assertTrue(in_array('abc123', $log['replaceVars'], true));

    $r = nexuspanel_ServiceSingleSignOn(testParams());
    assertSame(false, $r['success']);
}

function test_client_area_shows_the_server_or_a_notice(): void
{
    $node = new FakeNode();
    $node->on('GET', '/api/v1/provision/servers/srv-abc', 200, provisionedServer(['status' => 'suspended', 'install_state' => 'installed']));
    $node->on('GET', '/api/v1/provision/servers/srv-abc', 500, ['error' => 'boom']);
    $node->install();

    $out = nexuspanel_ClientArea(provisionedParams());
    assertSame('templates/overview.tpl', $out['tabOverviewReplacementTemplate']);
    $v = $out['templateVariables'];
    assertSame('203.0.113.10:20000', $v['address']);
    assertSame('Suspended', $v['statusLabel']);
    assertSame(true, $v['isSuspended']);
    assertSame('Installed', $v['installLabel']);
    assertSame(4.0, $v['memoryGb']);
    assertSame(2.0, $v['cpuCores']);
    assertSame('20', $v['maxPlayers']);
    assertSame('clientarea.php?action=productdetails&id=123&dosinglesignon=1', $v['ssoUrl']);
    assertSame(2, count($v['ports']));

    $out = nexuspanel_ClientArea(provisionedParams());
    assertSame('templates/error.tpl', $out['tabOverviewReplacementTemplate']);
    assertContains('could not be reached', $out['templateVariables']['error']);

    $out = nexuspanel_ClientArea(testParams());
    assertContains('not been provisioned', $out['templateVariables']['error']);
}

function test_admin_tab_fields_escape_output(): void
{
    $node = new FakeNode();
    $node->always('GET', '/api/v1/provision/servers/srv-abc', 200, provisionedServer(['name' => '<b>x</b>']));
    $node->install();
    $fields = nexuspanel_AdminServicesTabFields(provisionedParams());
    assertContains('srv-abc', $fields['Nexus server']);
    assertContains('/#/servers/srv-abc', $fields['Nexus server']);
    assertContains('Online', $fields['Nexus status']);
    assertSame('203.0.113.10:20000', $fields['Nexus address']);

    assertSame(['Nexus server' => 'Not provisioned'], nexuspanel_AdminServicesTabFields(testParams()));
}

function test_templates_exist_and_only_reference_provided_variables(): void
{
    $dir = __DIR__ . '/../modules/servers/nexuspanel/templates';
    $overview = file_get_contents($dir . '/overview.tpl');
    $error = file_get_contents($dir . '/error.tpl');
    assertTrue($overview !== false && $error !== false);

    $node = new FakeNode();
    $node->always('GET', '/api/v1/provision/servers/srv-abc', 200, provisionedServer());
    $node->install();
    $provided = array_keys(nexuspanel_ClientArea(provisionedParams())['templateVariables']);

    preg_match_all('/\{\$([a-zA-Z_]+)/', $overview, $m);
    foreach (array_unique($m[1]) as $var) {
        if ($var === 'p') {
            continue; // loop variable
        }
        assertTrue(in_array($var, $provided, true), "overview.tpl uses \$$var which ClientArea does not provide");
    }
    assertContains('{$error|escape}', $error);
}
