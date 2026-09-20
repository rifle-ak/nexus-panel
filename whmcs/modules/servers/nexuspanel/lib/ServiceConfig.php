<?php

declare(strict_types=1);

namespace WHMCS\Module\Server\NexusPanel;

/**
 * What a WHMCS product (plus the order's configurable options and custom
 * fields) says a server should be.
 *
 * Precedence, highest first: configurable options chosen on the order,
 * custom fields on the service, the product's module settings.
 */
final class ServiceConfig
{
    /** The shipped blueprints, for when the node cannot be asked. */
    public const SHIPPED_BLUEPRINTS = [
        'minecraft-paper' => 'Minecraft (Paper)',
        'rust' => 'Rust (Oxide)',
        'rust-carbon' => 'Rust (Carbon)',
        'valheim' => 'Valheim',
        'cs2' => 'Counter-Strike 2',
        'palworld' => 'Palworld',
        'dayz' => 'DayZ',
    ];

    public const DEFAULT_MEMORY_MB = 4096;
    public const DEFAULT_CPU_MILLICORES = 2000;
    public const DEFAULT_DISK_MB = 20480;
    public const DEFAULT_SLOTS = 20;
    public const DEFAULT_SESSION_HOURS = 8;

    public string $blueprint;
    public int $memoryMb;
    public int $cpuMillicores;
    public int $diskMb;
    public int $slots;
    /** @var array<string,string> */
    public array $variables;
    public bool $autoStart;
    public int $sessionHours;
    public string $name;

    /**
     * @param array<string,mixed> $params
     */
    public static function fromParams(array $params): self
    {
        $c = new self();

        // Module settings, in the order nexuspanel_ConfigOptions declares them.
        $c->blueprint = trim((string) ($params['configoption1'] ?? '')) ?: 'minecraft-paper';
        $c->memoryMb = self::int($params['configoption2'] ?? null, self::DEFAULT_MEMORY_MB);
        $c->cpuMillicores = self::int($params['configoption3'] ?? null, self::DEFAULT_CPU_MILLICORES);
        $c->diskMb = self::int($params['configoption4'] ?? null, self::DEFAULT_DISK_MB);
        $c->slots = self::int($params['configoption5'] ?? null, self::DEFAULT_SLOTS);
        $c->variables = self::parseVariables((string) ($params['configoption6'] ?? ''));
        $c->autoStart = !isset($params['configoption7']) || NexusClient::truthy($params['configoption7']);
        $c->sessionHours = self::int($params['configoption8'] ?? null, self::DEFAULT_SESSION_HOURS);

        // Configurable options the customer picked on the order.
        foreach ((array) ($params['configoptions'] ?? []) as $label => $value) {
            $c->applyOption((string) $label, $value);
        }

        // Custom fields on the service.
        $customName = '';
        foreach ((array) ($params['customfields'] ?? []) as $label => $value) {
            $key = self::normalise((string) $label);
            if (in_array($key, ['server_name', 'servername', 'name'], true)) {
                $customName = trim((string) $value);
            } else {
                $c->applyOption((string) $label, $value);
            }
        }

        $c->name = $customName !== '' ? $customName : self::defaultName($params, $c->blueprint);

        return $c;
    }

    /**
     * Apply one configurable option / custom field by its (loosely matched) name.
     */
    private function applyOption(string $label, $value): void
    {
        $key = self::normalise($label);
        $raw = trim((string) $value);
        if ($raw === '') {
            return;
        }

        switch ($key) {
            case 'memory':
            case 'memory_mb':
            case 'ram':
            case 'ram_mb':
                $this->memoryMb = self::int(self::stripUnit($raw, ['mb', 'mib']), $this->memoryMb);
                break;
            case 'memory_gb':
            case 'ram_gb':
                $this->memoryMb = (int) round((float) self::stripUnit($raw, ['gb', 'gib']) * 1024);
                break;
            case 'cpu':
            case 'cpu_millicores':
            case 'millicores':
                $this->cpuMillicores = self::int($raw, $this->cpuMillicores);
                break;
            case 'cores':
            case 'cpu_cores':
            case 'vcores':
            case 'vcpu':
                $this->cpuMillicores = (int) round((float) $raw * 1000);
                break;
            case 'disk':
            case 'disk_mb':
            case 'storage':
            case 'storage_mb':
                $this->diskMb = self::int(self::stripUnit($raw, ['mb', 'mib']), $this->diskMb);
                break;
            case 'disk_gb':
            case 'storage_gb':
                $this->diskMb = (int) round((float) self::stripUnit($raw, ['gb', 'gib']) * 1024);
                break;
            case 'slots':
            case 'players':
            case 'player_slots':
            case 'max_players':
                $this->slots = self::int($raw, $this->slots);
                break;
            case 'blueprint':
            case 'game':
                $this->blueprint = $raw;
                break;
            default:
                // Anything that looks like an environment variable name is
                // one: a "SERVER_PASSWORD" custom field sets that variable.
                if (preg_match('/^[A-Z][A-Z0-9_]*$/', $label)) {
                    $this->variables[$label] = $raw;
                }
        }
    }

    /**
     * `KEY=VALUE` lines into a map. Blank lines and `#` comments are skipped;
     * a line without `=` is ignored rather than sent as an empty variable.
     *
     * @return array<string,string>
     */
    public static function parseVariables(string $text): array
    {
        $out = [];
        foreach (preg_split('/\R/', $text) ?: [] as $line) {
            $line = trim($line);
            if ($line === '' || $line[0] === '#' || strpos($line, '=') === false) {
                continue;
            }
            [$key, $value] = explode('=', $line, 2);
            $key = trim($key);
            if (!preg_match('/^[A-Za-z_][A-Za-z0-9_]*$/', $key)) {
                continue;
            }
            $out[$key] = trim($value);
        }

        return $out;
    }

    /** The id the node uses to recognise this service on a retry. */
    public static function externalId(array $params): string
    {
        return 'whmcs-' . (int) ($params['serviceid'] ?? 0);
    }

    /** Who this is, for the node's logs. */
    public static function ownerRef(array $params): string
    {
        $client = (array) ($params['clientsdetails'] ?? []);
        $ref = 'client-' . (int) ($client['userid'] ?? $params['userid'] ?? 0);
        $email = trim((string) ($client['email'] ?? ''));

        return $email !== '' ? $ref . ' <' . $email . '>' : $ref;
    }

    /** What the node is told to create. */
    public function toCreatePayload(array $params): array
    {
        return [
            'external_id' => self::externalId($params),
            'name' => $this->name,
            'blueprint' => $this->blueprint,
            'memory_mb' => $this->memoryMb,
            'cpu_millicores' => $this->cpuMillicores,
            'disk_mb' => $this->diskMb,
            'variables' => $this->allVariables(),
            'auto_start' => $this->autoStart,
            'owner' => self::ownerRef($params),
        ];
    }

    /** What the node is told on an upgrade/downgrade. Ports and name stay. */
    public function toPackagePayload(): array
    {
        return [
            'memory_mb' => $this->memoryMb,
            'cpu_millicores' => $this->cpuMillicores,
            'disk_mb' => $this->diskMb,
            'variables' => $this->allVariables(),
        ];
    }

    /** Product variables plus the slot count as MAX_PLAYERS (unless set by hand). */
    public function allVariables(): array
    {
        $vars = $this->variables;
        if (!isset($vars['MAX_PLAYERS']) && $this->slots > 0) {
            $vars['MAX_PLAYERS'] = (string) $this->slots;
        }

        return $vars;
    }

    public function sessionTtlSecs(): int
    {
        return max(1, $this->sessionHours) * 3600;
    }

    private static function defaultName(array $params, string $blueprint): string
    {
        $domain = trim((string) ($params['domain'] ?? ''));
        if ($domain !== '') {
            return $domain;
        }
        $client = (array) ($params['clientsdetails'] ?? []);
        $first = trim((string) ($client['firstname'] ?? ''));
        $game = self::SHIPPED_BLUEPRINTS[$blueprint] ?? ucfirst($blueprint);
        if ($first !== '') {
            return $first . "'s " . $game . ' server';
        }

        return $game . ' server #' . (int) ($params['serviceid'] ?? 0);
    }

    /**
     * "Memory (GB)" → "memory_gb", "CPU Cores" → "cpu_cores": a unit in
     * brackets is part of the meaning, not decoration.
     */
    private static function normalise(string $label): string
    {
        $key = strtolower(trim($label));
        $key = preg_replace('/[^a-z0-9]+/', '_', $key) ?? $key;

        return trim($key, '_');
    }

    private static function stripUnit(string $raw, array $units): string
    {
        $lower = strtolower($raw);
        foreach ($units as $unit) {
            if (substr($lower, -strlen($unit)) === $unit) {
                return trim(substr($raw, 0, -strlen($unit)));
            }
        }

        return $raw;
    }

    private static function int($value, int $default): int
    {
        if ($value === null) {
            return $default;
        }
        $v = trim((string) $value);
        if ($v === '' || !is_numeric($v)) {
            return $default;
        }
        $n = (int) round((float) $v);

        return $n > 0 ? $n : $default;
    }
}
