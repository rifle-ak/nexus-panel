<?php
/**
 * Nexus Panel provisioning module for WHMCS.
 *
 * Creates, suspends, upgrades and terminates game servers on a Nexus node
 * as WHMCS orders come and go, gives customers a one-click sign-in to their
 * server, and reports disk usage back for WHMCS's usage stats.
 *
 * Setup: docs/WHMCS.md in the Nexus Panel repository.
 *
 * @see https://developers.whmcs.com/provisioning-modules/
 */

declare(strict_types=1);

use WHMCS\Module\Server\NexusPanel\NexusApiException;
use WHMCS\Module\Server\NexusPanel\NexusClient;
use WHMCS\Module\Server\NexusPanel\ServiceConfig;
use WHMCS\Module\Server\NexusPanel\ServiceRecord;

if (!defined('WHMCS')) {
    // Loaded outside WHMCS (tests, a lint). The classes are still usable;
    // the module functions below just have nothing to be called by.
}

require_once __DIR__ . '/lib/NexusApiException.php';
require_once __DIR__ . '/lib/NexusClient.php';
require_once __DIR__ . '/lib/ServiceConfig.php';
require_once __DIR__ . '/lib/ServiceRecord.php';

// ─────────────────────────────────────────────────────────────────────
// Module metadata and product configuration
// ─────────────────────────────────────────────────────────────────────

function nexuspanel_MetaData(): array
{
    return [
        'DisplayName' => 'Nexus Panel',
        'APIVersion' => '1.1',
        'RequiresServer' => true,
        'DefaultNonSSLPort' => (string) NexusClient::DEFAULT_HTTP_PORT,
        'DefaultSSLPort' => (string) NexusClient::DEFAULT_HTTPS_PORT,
        'ServiceSingleSignOnLabel' => 'Open game panel',
    ];
}

/**
 * Product settings (Products/Services → Module Settings). Order matters:
 * WHMCS hands them back as configoption1..8, which ServiceConfig reads.
 */
function nexuspanel_ConfigOptions(): array
{
    return [
        'Blueprint' => [
            'Type' => 'dropdown',
            'Loader' => 'nexuspanel_BlueprintLoader',
            'SimpleMode' => true,
            'Description' => 'The game this product provisions.',
        ],
        'Memory (MB)' => [
            'Type' => 'text',
            'Size' => '8',
            'Default' => (string) ServiceConfig::DEFAULT_MEMORY_MB,
            'SimpleMode' => true,
            'Description' => 'Hard memory limit for the server container.',
        ],
        'CPU (millicores)' => [
            'Type' => 'text',
            'Size' => '8',
            'Default' => (string) ServiceConfig::DEFAULT_CPU_MILLICORES,
            'SimpleMode' => true,
            'Description' => '1000 = one full core.',
        ],
        'Disk (MB)' => [
            'Type' => 'text',
            'Size' => '8',
            'Default' => (string) ServiceConfig::DEFAULT_DISK_MB,
            'SimpleMode' => true,
            'Description' => 'Disk allowance; reported as the service\'s disk limit.',
        ],
        'Player slots' => [
            'Type' => 'text',
            'Size' => '5',
            'Default' => (string) ServiceConfig::DEFAULT_SLOTS,
            'SimpleMode' => true,
            'Description' => 'Sets the blueprint\'s MAX_PLAYERS variable.',
        ],
        'Variables' => [
            'Type' => 'textarea',
            'Rows' => '4',
            'Cols' => '40',
            'Description' => 'One KEY=VALUE per line, applied to the blueprint (e.g. SERVER_NAME=My Host, VIEW_DISTANCE=8).',
        ],
        'Start after install' => [
            'Type' => 'yesno',
            'Default' => 'on',
            'Description' => 'Start the server as soon as its game files are installed.',
        ],
        'Panel session (hours)' => [
            'Type' => 'text',
            'Size' => '5',
            'Default' => (string) ServiceConfig::DEFAULT_SESSION_HOURS,
            'Description' => 'How long a customer stays signed in to the panel after clicking through from the client area.',
        ],
    ];
}

/**
 * Blueprint dropdown contents: what the product's server group actually
 * ships, falling back to the well-known set when no node is reachable yet
 * (the product may be configured before its server is).
 */
function nexuspanel_BlueprintLoader(array $params): array
{
    try {
        $client = nexuspanel_client($params);
        $options = [];
        foreach ($client->blueprints() as $bp) {
            $id = (string) ($bp['id'] ?? '');
            if ($id === '') {
                continue;
            }
            $label = (string) ($bp['name'] ?? $id);
            $game = (string) ($bp['game'] ?? '');
            $options[$id] = $game !== '' && stripos($label, $game) === false ? "$label ($game)" : $label;
        }
        if ($options !== []) {
            return $options;
        }
    } catch (\Throwable $e) {
        // Fall through to the shipped list.
    }

    return ServiceConfig::SHIPPED_BLUEPRINTS;
}

// ─────────────────────────────────────────────────────────────────────
// Server connection
// ─────────────────────────────────────────────────────────────────────

function nexuspanel_TestConnection(array $params): array
{
    try {
        $info = nexuspanel_client($params)->nodeInfo();
        nexuspanel_log($params, 'TestConnection', ['url' => NexusClient::baseUrlFromParams($params)], $info);

        return ['success' => true, 'error' => ''];
    } catch (\Throwable $e) {
        nexuspanel_log($params, 'TestConnection', [], $e->getMessage());

        return ['success' => false, 'error' => $e->getMessage()];
    }
}

// ─────────────────────────────────────────────────────────────────────
// Lifecycle
// ─────────────────────────────────────────────────────────────────────

function nexuspanel_CreateAccount(array $params): string
{
    try {
        $client = nexuspanel_client($params);
        $config = ServiceConfig::fromParams($params);
        $payload = $config->toCreatePayload($params);

        $server = $client->provision($payload);
        nexuspanel_log($params, 'CreateAccount', $payload, $server);

        $serverId = (string) ($server['id'] ?? '');
        if ($serverId === '') {
            return 'The node created a server but returned no id';
        }

        $ip = (string) ($server['ip'] ?? '');
        if ($ip === '') {
            // The node does not know its public address; WHMCS's server
            // record does.
            $ip = trim((string) ($params['serverip'] ?? ''));
        }
        $port = isset($server['primary_port']) ? (string) $server['primary_port'] : '';

        ServiceRecord::store($params, [
            ServiceRecord::SERVER_ID => $serverId,
            ServiceRecord::SERVER_IP => $ip,
            ServiceRecord::SERVER_PORT => $port,
        ]);
        if ($ip !== '') {
            ServiceRecord::setDedicatedIp($params, $port !== '' ? "$ip:$port" : $ip);
        }

        return 'success';
    } catch (\Throwable $e) {
        nexuspanel_log($params, 'CreateAccount', $params['configoptions'] ?? [], $e->getMessage());

        return $e->getMessage();
    }
}

function nexuspanel_SuspendAccount(array $params): string
{
    return nexuspanel_withServer($params, 'SuspendAccount', function (NexusClient $client, string $id): void {
        $client->suspend($id);
    });
}

function nexuspanel_UnsuspendAccount(array $params): string
{
    return nexuspanel_withServer($params, 'UnsuspendAccount', function (NexusClient $client, string $id): void {
        $client->unsuspend($id);
        // Unsuspending only clears the flag; the customer paid for a running
        // server, so start it again.
        try {
            $client->start($id);
        } catch (NexusApiException $e) {
            // A server whose game files are not installed cannot start; the
            // customer can install from the panel. Not a failed unsuspension.
        }
    });
}

function nexuspanel_TerminateAccount(array $params): string
{
    $serverId = ServiceRecord::serverId($params);
    if ($serverId === null) {
        // Nothing was ever provisioned (or it was already terminated).
        return 'success';
    }
    try {
        $client = nexuspanel_client($params);
        $result = $client->terminate($serverId);
        nexuspanel_log($params, 'TerminateAccount', ['server_id' => $serverId], $result);
        ServiceRecord::store($params, [
            ServiceRecord::SERVER_ID => '',
            ServiceRecord::SERVER_IP => '',
            ServiceRecord::SERVER_PORT => '',
        ]);

        return 'success';
    } catch (\Throwable $e) {
        nexuspanel_log($params, 'TerminateAccount', ['server_id' => $serverId], $e->getMessage());

        return $e->getMessage();
    }
}

function nexuspanel_ChangePackage(array $params): string
{
    return nexuspanel_withServer($params, 'ChangePackage', function (NexusClient $client, string $id) use ($params): void {
        $client->changePackage($id, ServiceConfig::fromParams($params)->toPackagePayload());
    });
}

// ─────────────────────────────────────────────────────────────────────
// Custom buttons (admin and client area)
// ─────────────────────────────────────────────────────────────────────

function nexuspanel_AdminCustomButtonArray(): array
{
    return [
        'Start' => 'start',
        'Stop' => 'stop',
        'Restart' => 'restart',
        'Reinstall game files' => 'reinstall',
    ];
}

function nexuspanel_ClientAreaCustomButtonArray(): array
{
    return [
        'Start' => 'start',
        'Stop' => 'stop',
        'Restart' => 'restart',
    ];
}

function nexuspanel_start(array $params): string
{
    return nexuspanel_withServer($params, 'start', function (NexusClient $client, string $id): void {
        $client->start($id);
    });
}

function nexuspanel_stop(array $params): string
{
    return nexuspanel_withServer($params, 'stop', function (NexusClient $client, string $id): void {
        $client->stop($id);
    });
}

function nexuspanel_restart(array $params): string
{
    return nexuspanel_withServer($params, 'restart', function (NexusClient $client, string $id): void {
        $client->restart($id);
    });
}

function nexuspanel_reinstall(array $params): string
{
    return nexuspanel_withServer($params, 'reinstall', function (NexusClient $client, string $id): void {
        $client->reinstall($id);
    });
}

// ─────────────────────────────────────────────────────────────────────
// Single sign-on
// ─────────────────────────────────────────────────────────────────────

/**
 * The client area's "Open game panel" button: mint a one-time link on the
 * node and send the browser there. The resulting panel session is scoped
 * to this one server.
 */
function nexuspanel_ServiceSingleSignOn(array $params): array
{
    $serverId = ServiceRecord::serverId($params);
    if ($serverId === null) {
        return ['success' => false, 'errorMsg' => 'This service has no server provisioned yet.'];
    }
    try {
        $client = nexuspanel_client($params);
        $config = ServiceConfig::fromParams($params);
        $sso = $client->ssoToken($serverId, ServiceConfig::ownerRef($params), $config->sessionTtlSecs());
        nexuspanel_log($params, 'ServiceSingleSignOn', ['server_id' => $serverId], ['expires_in_secs' => $sso['expires_in_secs'] ?? null], [$sso['token'] ?? '']);

        return ['success' => true, 'redirectTo' => $client->ssoUrl((string) $sso['path'])];
    } catch (\Throwable $e) {
        nexuspanel_log($params, 'ServiceSingleSignOn', ['server_id' => $serverId], $e->getMessage());

        return ['success' => false, 'errorMsg' => $e->getMessage()];
    }
}

// ─────────────────────────────────────────────────────────────────────
// Client and admin area
// ─────────────────────────────────────────────────────────────────────

function nexuspanel_ClientArea(array $params): array
{
    $serverId = ServiceRecord::serverId($params);
    $serviceId = (int) ($params['serviceid'] ?? 0);
    $vars = [
        'serviceId' => $serviceId,
        'ssoUrl' => 'clientarea.php?action=productdetails&id=' . $serviceId . '&dosinglesignon=1',
    ];

    if ($serverId === null) {
        $vars['error'] = 'Your server has not been provisioned yet. It will appear here once your order is active.';

        return nexuspanel_clientTemplate('error', $vars);
    }

    try {
        $server = nexuspanel_client($params)->server($serverId);
    } catch (\Throwable $e) {
        $vars['error'] = 'The game panel could not be reached right now. Please try again in a moment.';
        nexuspanel_log($params, 'ClientArea', ['server_id' => $serverId], $e->getMessage());

        return nexuspanel_clientTemplate('error', $vars);
    }

    return nexuspanel_clientTemplate('overview', $vars + nexuspanel_serverView($params, $server));
}

function nexuspanel_AdminServicesTabFields(array $params): array
{
    $serverId = ServiceRecord::serverId($params);
    if ($serverId === null) {
        return ['Nexus server' => 'Not provisioned'];
    }
    try {
        $server = nexuspanel_client($params)->server($serverId);
    } catch (\Throwable $e) {
        return [
            'Nexus server' => htmlspecialchars($serverId, ENT_QUOTES, 'UTF-8'),
            'Nexus status' => 'Unavailable: ' . htmlspecialchars($e->getMessage(), ENT_QUOTES, 'UTF-8'),
        ];
    }
    $view = nexuspanel_serverView($params, $server);
    $panel = NexusClient::baseUrlFromParams($params) . '/#/servers/' . rawurlencode($serverId);
    $h = static fn ($v): string => htmlspecialchars((string) $v, ENT_QUOTES, 'UTF-8');

    return [
        'Nexus server' => '<a href="' . $h($panel) . '" target="_blank" rel="noopener">' . $h($serverId) . '</a>',
        'Nexus status' => $h($view['statusLabel']) . ' — game files: ' . $h($view['installLabel']),
        'Nexus address' => $h($view['address']),
        'Nexus resources' => $h($view['memoryGb']) . ' GB RAM, ' . $h($view['cpuCores']) . ' cores, ' . $h($view['diskGb']) . ' GB disk',
    ];
}

/**
 * Nightly usage import (Setup → Servers → "Update Usage Statistics" and
 * the daily cron). Called once per server, so this walks every active
 * Nexus service on it.
 */
function nexuspanel_UsageUpdate(array $params): void
{
    if (!class_exists('\WHMCS\Database\Capsule') || !class_exists('\WHMCS\Service\Service')) {
        return;
    }
    $serverRecordId = (int) ($params['serverid'] ?? 0);
    if ($serverRecordId <= 0) {
        return;
    }

    try {
        $client = nexuspanel_client($params);
    } catch (\Throwable $e) {
        nexuspanel_log($params, 'UsageUpdate', [], $e->getMessage());

        return;
    }

    $services = \WHMCS\Database\Capsule::table('tblhosting')
        ->join('tblproducts', 'tblproducts.id', '=', 'tblhosting.packageid')
        ->where('tblhosting.server', $serverRecordId)
        ->where('tblproducts.servertype', 'nexuspanel')
        ->whereIn('tblhosting.domainstatus', ['Active', 'Suspended'])
        ->select('tblhosting.id')
        ->get();

    foreach ($services as $row) {
        $service = \WHMCS\Service\Service::find((int) $row->id);
        if ($service === null) {
            continue;
        }
        $serverId = (string) $service->serviceProperties->get(ServiceRecord::SERVER_ID);
        if ($serverId === '') {
            continue;
        }
        try {
            $usage = $client->usage($serverId);
            \WHMCS\Database\Capsule::table('tblhosting')
                ->where('id', (int) $row->id)
                ->update([
                    'diskusage' => (int) round(((int) ($usage['disk_used_bytes'] ?? 0)) / 1048576),
                    'disklimit' => (int) round(((int) ($usage['disk_limit_bytes'] ?? 0)) / 1048576),
                    'lastupdate' => date('Y-m-d H:i:s'),
                ]);
        } catch (\Throwable $e) {
            nexuspanel_log($params, 'UsageUpdate', ['service' => (int) $row->id, 'server_id' => $serverId], $e->getMessage());
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/** @internal Swapped by tests to inject a fake transport. */
function nexuspanel_client(array $params): NexusClient
{
    $factory = $GLOBALS['nexuspanel_client_factory'] ?? null;
    if (is_callable($factory)) {
        return $factory($params);
    }

    return NexusClient::fromParams($params);
}

/**
 * Run an action against this service's server, turning the outcome into
 * the string WHMCS expects: 'success' or an error message.
 */
function nexuspanel_withServer(array $params, string $action, callable $fn): string
{
    $serverId = ServiceRecord::serverId($params);
    if ($serverId === null) {
        return 'This service has no server provisioned yet.';
    }
    try {
        $client = nexuspanel_client($params);
        $fn($client, $serverId);
        nexuspanel_log($params, $action, ['server_id' => $serverId], 'success');

        return 'success';
    } catch (\Throwable $e) {
        nexuspanel_log($params, $action, ['server_id' => $serverId], $e->getMessage());

        return $e->getMessage();
    }
}

/**
 * Flatten a node server record into what the templates show.
 *
 * @param array<string,mixed> $server
 * @return array<string,mixed>
 */
function nexuspanel_serverView(array $params, array $server): array
{
    $ip = (string) ($server['ip'] ?? '');
    if ($ip === '') {
        $ip = ServiceRecord::get($params, ServiceRecord::SERVER_IP) ?? trim((string) ($params['serverip'] ?? ''));
    }
    $port = isset($server['primary_port']) ? (string) $server['primary_port'] : (ServiceRecord::get($params, ServiceRecord::SERVER_PORT) ?? '');
    $status = (string) ($server['status'] ?? 'unknown');
    $install = (string) ($server['install_state'] ?? 'unknown');
    $resources = (array) ($server['resources'] ?? []);

    $statusLabels = [
        'running' => 'Online',
        'stopped' => 'Offline',
        'created' => 'Offline',
        'failed' => 'Crashed',
        'paused' => 'Paused',
        'suspended' => 'Suspended',
    ];
    $installLabels = [
        'not_required' => 'Ready',
        'installed' => 'Installed',
        'pending' => 'Waiting to install',
        'running' => 'Installing…',
        'failed' => 'Install failed',
        'unknown' => 'Unknown',
    ];

    $ports = [];
    foreach ((array) ($server['ports'] ?? []) as $p) {
        $ports[] = [
            'name' => (string) ($p['name'] ?? ''),
            'port' => (string) ($p['port'] ?? ''),
            'protocol' => strtoupper((string) ($p['protocol'] ?? '')),
        ];
    }

    return [
        'serverId' => (string) ($server['id'] ?? ''),
        'serverName' => (string) ($server['name'] ?? ''),
        'game' => (string) ($server['game'] ?? ''),
        'blueprint' => (string) ($server['blueprint'] ?? ''),
        'status' => $status,
        'statusLabel' => $statusLabels[$status] ?? ucfirst($status),
        'isRunning' => $status === 'running',
        'isSuspended' => $status === 'suspended',
        'installState' => $install,
        'installLabel' => $installLabels[$install] ?? ucfirst($install),
        'installing' => !empty($server['installing']),
        'ip' => $ip,
        'port' => $port,
        'address' => $port !== '' ? "$ip:$port" : $ip,
        'ports' => $ports,
        'memoryGb' => round(((int) ($resources['memory_mb'] ?? 0)) / 1024, 1),
        'cpuCores' => round(((int) ($resources['cpu_millicores'] ?? 0)) / 1000, 1),
        'diskGb' => round(((int) ($resources['disk_mb'] ?? 0)) / 1024, 1),
        'maxPlayers' => (string) (($server['variables'] ?? [])['MAX_PLAYERS'] ?? ''),
    ];
}

function nexuspanel_clientTemplate(string $template, array $vars): array
{
    return [
        'tabOverviewReplacementTemplate' => 'templates/' . $template . '.tpl',
        'templateVariables' => $vars,
    ];
}

/**
 * Write to WHMCS's module log (Utilities → Logs → Module Log). Secrets are
 * masked: the node API key always, plus anything the caller names.
 *
 * @param mixed $request
 * @param mixed $response
 * @param string[] $secrets
 */
function nexuspanel_log(array $params, string $action, $request, $response, array $secrets = []): void
{
    if (!function_exists('logModuleCall')) {
        return;
    }
    foreach (['serveraccesshash', 'serverpassword'] as $key) {
        $value = (string) ($params[$key] ?? '');
        if ($value !== '') {
            $secrets[] = $value;
        }
    }
    logModuleCall('nexuspanel', $action, $request, $response, '', array_values(array_filter($secrets)));
}
