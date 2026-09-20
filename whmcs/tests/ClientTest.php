<?php

declare(strict_types=1);

use WHMCS\Module\Server\NexusPanel\NexusApiException;
use WHMCS\Module\Server\NexusPanel\NexusClient;

function test_base_url_follows_the_server_record(): void
{
    assertSame('https://node1.example.test', NexusClient::baseUrlFromParams(testParams()));
    assertSame('https://node1.example.test:8443', NexusClient::baseUrlFromParams(testParams(['serverport' => '8443'])));
    assertSame('http://node1.example.test:3000', NexusClient::baseUrlFromParams(testParams(['serversecure' => '', 'serverport' => ''])));
    assertSame('http://node1.example.test', NexusClient::baseUrlFromParams(testParams(['serversecure' => '', 'serverport' => '80'])));
    assertSame('https://198.51.100.7', NexusClient::baseUrlFromParams(testParams(['serverhostname' => ''])));
}

function test_base_url_rejects_hostnames_that_are_not_hostnames(): void
{
    try {
        NexusClient::baseUrlFromParams(testParams(['serverhostname' => 'evil.test/../../']));
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertContains('cannot be part of a URL', $e->getMessage());
    }
    try {
        NexusClient::baseUrlFromParams(testParams(['serverhostname' => '', 'serverip' => '']));
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertContains('neither a hostname nor an IP', $e->getMessage());
    }
}

function test_api_key_comes_from_access_hash_then_password(): void
{
    $node = new FakeNode();
    $node->always('GET', '/api/v1/node/info', 200, ['node_id' => 'n1']);

    NexusClient::fromParams(testParams(), $node->transport())->nodeInfo();
    assertTrue(in_array('X-Api-Key: node-api-key-secret', $node->requests[0]['headers'], true), 'access hash used');

    NexusClient::fromParams(testParams(['serveraccesshash' => '', 'serverpassword' => 'pw-key']), $node->transport())->nodeInfo();
    assertTrue(in_array('X-Api-Key: pw-key', $node->requests[1]['headers'], true), 'password used as fallback');

    try {
        NexusClient::fromParams(testParams(['serveraccesshash' => '', 'serverpassword' => '']));
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertContains('Access Hash', $e->getMessage());
    }
}

function test_node_errors_become_exceptions_with_the_nodes_message(): void
{
    $node = new FakeNode();
    $node->on('POST', '/api/v1/provision/servers', 409, ['error' => 'requested port 25565 is already in use']);
    $node->on('POST', '/api/v1/provision/servers', 502, '<html>bad gateway</html>');
    $client = NexusClient::fromParams(testParams(), $node->transport());

    try {
        $client->provision(['external_id' => 'x']);
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertSame(409, $e->getStatus());
        assertSame('requested port 25565 is already in use', $e->getMessage());
    }
    try {
        $client->provision(['external_id' => 'x']);
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertSame(502, $e->getStatus());
        assertContains('HTTP 502', $e->getMessage());
    }
}

function test_non_json_success_is_an_error_not_a_silent_empty_result(): void
{
    $node = new FakeNode();
    $node->on('GET', '/api/v1/node/info', 200, '<!DOCTYPE html><html>a login page</html>');
    $client = NexusClient::fromParams(testParams(), $node->transport());
    try {
        $client->nodeInfo();
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertContains('not JSON', $e->getMessage());
    }
}

function test_transport_failures_are_wrapped(): void
{
    $client = new NexusClient('https://down.example.test', 'k', static function (): array {
        throw new \RuntimeException('connection refused');
    });
    try {
        $client->nodeInfo();
        throw new TestFailure('expected an exception');
    } catch (NexusApiException $e) {
        assertSame(0, $e->getStatus());
        assertContains('connection refused', $e->getMessage());
        assertContains('down.example.test', $e->getMessage());
    }
}

function test_requests_hit_the_right_paths_with_encoded_ids(): void
{
    $node = new FakeNode();
    $node->always('POST', '/api/v1/containers/a%2Fb/suspend', 200, ['ok' => true]);
    $node->always('DELETE', '/api/v1/provision/servers/a%2Fb', 200, ['ok' => true, 'existed' => true]);
    $node->always('GET', '/api/v1/provision/servers', 200, [provisionedServer()]);
    $node->always('POST', '/api/v1/provision/sso', 200, ['token' => 't', 'path' => '/sso/t', 'expires_in_secs' => 60]);
    $client = NexusClient::fromParams(testParams(), $node->transport());

    $client->suspend('a/b');
    $client->terminate('a/b');
    $found = $client->findByExternalId('whmcs-123');
    assertSame('srv-abc', $found['id']);
    assertContains('external_id=whmcs-123', $node->requests[2]['url']);

    $sso = $client->ssoToken('srv-abc', 'client-42', 3600);
    assertSame('https://node1.example.test/sso/t', $client->ssoUrl($sso['path']));
    $body = json_decode($node->requests[3]['body'], true);
    assertSame(['server_id' => 'srv-abc', 'subject' => 'client-42', 'session_ttl_secs' => 3600], $body);
}

function test_blueprints_are_read_from_the_node(): void
{
    $node = new FakeNode();
    $node->always('GET', '/api/v1/blueprints', 200, [
        ['id' => 'rust', 'name' => 'Rust Dedicated Server', 'game' => 'rust'],
        ['id' => 'dayz', 'name' => 'DayZ', 'game' => 'dayz'],
    ]);
    $client = NexusClient::fromParams(testParams(), $node->transport());
    $ids = array_column($client->blueprints(), 'id');
    assertSame(['rust', 'dayz'], $ids);
}
