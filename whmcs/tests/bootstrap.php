<?php
/**
 * Test bootstrap: a minimal stand-in for the WHMCS runtime plus a fake
 * Nexus node, so the module's functions can be exercised end to end
 * without either.
 */

declare(strict_types=1);

if (!defined('WHMCS')) {
    define('WHMCS', true);
}

/** Every module-log call, for assertions. @var array<int,array> */
$GLOBALS['nexuspanel_test_log'] = [];

function logModuleCall(string $module, string $action, $request, $response, $processed = '', array $replaceVars = []): void
{
    $GLOBALS['nexuspanel_test_log'][] = compact('module', 'action', 'request', 'response', 'replaceVars');
}

require_once __DIR__ . '/../modules/servers/nexuspanel/nexuspanel.php';

/** Stand-in for WHMCS's service properties accessor. */
final class FakeServiceProperties
{
    /** @var array<string,string> */
    public array $values = [];

    public function get(string $name)
    {
        return $this->values[$name] ?? null;
    }

    public function save(array $values): void
    {
        foreach ($values as $k => $v) {
            $this->values[(string) $k] = (string) $v;
        }
    }
}

final class FakeServiceModel
{
    public FakeServiceProperties $serviceProperties;

    public function __construct()
    {
        $this->serviceProperties = new FakeServiceProperties();
    }
}

/**
 * A scripted Nexus node: a queue of responses per "METHOD path" and a
 * record of every request made.
 */
final class FakeNode
{
    /** @var array<int,array{method:string,url:string,headers:string[],body:?string}> */
    public array $requests = [];
    /** @var array<string,array<int,array{0:int,1:string}>> */
    private array $responses = [];
    /** @var array<string,array{0:int,1:string}> */
    private array $defaults = [];

    public function on(string $method, string $path, int $status, $body): self
    {
        $this->responses["$method $path"][] = [$status, is_string($body) ? $body : json_encode($body)];

        return $this;
    }

    public function always(string $method, string $path, int $status, $body): self
    {
        $this->defaults["$method $path"] = [$status, is_string($body) ? $body : json_encode($body)];

        return $this;
    }

    public function transport(): callable
    {
        return function (string $method, string $url, array $headers, ?string $body): array {
            $this->requests[] = compact('method', 'url', 'headers', 'body');
            $path = parse_url($url, PHP_URL_PATH) ?: '/';
            $query = parse_url($url, PHP_URL_QUERY);
            $key = "$method $path" . ($query ? "?$query" : '');
            $bare = "$method $path";
            foreach ([$key, $bare] as $k) {
                if (!empty($this->responses[$k])) {
                    return array_shift($this->responses[$k]);
                }
                if (isset($this->defaults[$k])) {
                    return $this->defaults[$k];
                }
            }

            return [404, json_encode(['error' => "fake node has no route for $key"])];
        };
    }

    /** Install this node as what nexuspanel_client() hands out. */
    public function install(): void
    {
        $transport = $this->transport();
        $GLOBALS['nexuspanel_client_factory'] = static function (array $params) use ($transport) {
            return \WHMCS\Module\Server\NexusPanel\NexusClient::fromParams($params, $transport);
        };
    }

    public function lastBody(): ?array
    {
        $last = end($this->requests);
        if ($last === false || $last['body'] === null) {
            return null;
        }

        return json_decode($last['body'], true);
    }
}

/** A typical set of module params for one service. */
function testParams(array $overrides = []): array
{
    return array_replace([
        'serviceid' => 123,
        'pid' => 7,
        'userid' => 42,
        'domain' => '',
        'serverid' => 3,
        'serverhostname' => 'node1.example.test',
        'serverip' => '198.51.100.7',
        'serverusername' => '',
        'serverpassword' => '',
        'serveraccesshash' => 'node-api-key-secret',
        'serversecure' => 'on',
        'serverport' => '443',
        'configoption1' => 'minecraft-paper',
        'configoption2' => '4096',
        'configoption3' => '2000',
        'configoption4' => '20480',
        'configoption5' => '20',
        'configoption6' => "VIEW_DISTANCE=8\n# comment\nSERVER_NAME=Example Host",
        'configoption7' => 'on',
        'configoption8' => '8',
        'configoptions' => [],
        'customfields' => [],
        'clientsdetails' => ['userid' => 42, 'firstname' => 'Ryan', 'lastname' => 'C', 'email' => 'ryan@example.test'],
        'model' => new FakeServiceModel(),
    ], $overrides);
}

function provisionedServer(array $overrides = []): array
{
    return array_replace([
        'id' => 'srv-abc',
        'external_id' => 'whmcs-123',
        'name' => "Ryan's Minecraft (Paper) server",
        'blueprint' => 'minecraft-paper',
        'game' => 'minecraft',
        'resources' => ['memory_mb' => 4096, 'cpu_millicores' => 2000, 'disk_mb' => 20480],
        'ports' => [
            ['name' => 'game', 'port' => 20000, 'protocol' => 'tcp', 'variable' => 'SERVER_PORT'],
            ['name' => 'rcon', 'port' => 20001, 'protocol' => 'tcp', 'variable' => 'RCON_PORT'],
        ],
        'primary_port' => 20000,
        'ip' => '203.0.113.10',
        'variables' => ['MAX_PLAYERS' => '20'],
        'status' => 'running',
        'install_state' => 'installed',
        'installing' => false,
        'created_at' => 1,
        'updated_at' => 1,
    ], $overrides);
}

// ── Tiny assertion kit ─────────────────────────────────────────────

final class TestFailure extends \Exception
{
}

function assertSame($expected, $actual, string $what = ''): void
{
    if ($expected !== $actual) {
        throw new TestFailure(sprintf(
            "%sexpected %s, got %s",
            $what !== '' ? "$what: " : '',
            var_export($expected, true),
            var_export($actual, true)
        ));
    }
}

function assertTrue($value, string $what = ''): void
{
    assertSame(true, (bool) $value, $what);
}

function assertContains(string $needle, string $haystack, string $what = ''): void
{
    if (strpos($haystack, $needle) === false) {
        throw new TestFailure(sprintf("%s%s not found in %s", $what !== '' ? "$what: " : '', var_export($needle, true), var_export($haystack, true)));
    }
}
