<?php

declare(strict_types=1);

namespace WHMCS\Module\Server\NexusPanel;

/**
 * Where the module keeps what it learned about a service: the node's id for
 * the server and the address it was given.
 *
 * Stored as WHMCS service properties (admin-only custom fields the platform
 * creates on demand), so they survive without a schema of our own and show
 * up on the admin's service page.
 */
final class ServiceRecord
{
    public const SERVER_ID = 'Server ID';
    public const SERVER_IP = 'Server IP';
    public const SERVER_PORT = 'Server Port';

    /**
     * The node's id for this service's server, or null if none was provisioned.
     *
     * @param array<string,mixed> $params
     */
    public static function serverId(array $params): ?string
    {
        $id = self::get($params, self::SERVER_ID);

        return $id !== null && $id !== '' ? $id : null;
    }

    /**
     * @param array<string,mixed> $params
     */
    public static function get(array $params, string $name): ?string
    {
        $props = self::properties($params);
        if ($props !== null) {
            $value = $props->get($name);
            if ($value !== null && $value !== '') {
                return (string) $value;
            }
        }
        // Custom fields WHMCS already loaded into the params.
        $fields = (array) ($params['customfields'] ?? []);
        if (isset($fields[$name]) && $fields[$name] !== '') {
            return (string) $fields[$name];
        }

        return null;
    }

    /**
     * @param array<string,mixed> $params
     * @param array<string,string> $values
     */
    public static function store(array $params, array $values): void
    {
        $props = self::properties($params);
        if ($props === null) {
            throw new NexusApiException(
                'WHMCS did not pass a service model; this module needs WHMCS 7.0 or newer'
            );
        }
        $props->save($values);
    }

    /**
     * Update the address WHMCS shows for the service (the "Dedicated IP"
     * column), best-effort: it is cosmetic, and an older WHMCS without the
     * database layer must not fail provisioning over it.
     *
     * @param array<string,mixed> $params
     */
    public static function setDedicatedIp(array $params, string $address): void
    {
        $serviceId = (int) ($params['serviceid'] ?? 0);
        if ($serviceId <= 0 || !class_exists('\WHMCS\Database\Capsule')) {
            return;
        }
        try {
            \WHMCS\Database\Capsule::table('tblhosting')
                ->where('id', $serviceId)
                ->update(['dedicatedip' => $address]);
        } catch (\Throwable $e) {
            // Cosmetic; leave it.
        }
    }

    /**
     * @param array<string,mixed> $params
     * @return object|null Something with get(string) and save(array).
     */
    private static function properties(array $params)
    {
        $model = $params['model'] ?? null;
        if (!is_object($model)) {
            return null;
        }
        $props = $model->serviceProperties ?? null;
        if (is_object($props) && method_exists($props, 'get') && method_exists($props, 'save')) {
            return $props;
        }

        return null;
    }
}
