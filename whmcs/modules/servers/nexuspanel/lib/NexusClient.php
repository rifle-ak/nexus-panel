<?php

declare(strict_types=1);

namespace WHMCS\Module\Server\NexusPanel;

/**
 * HTTP client for a Nexus node's REST API.
 *
 * Authenticates with the node API key WHMCS stores in the server's
 * "Access Hash" field, verifies TLS, never follows redirects, and turns every
 * failure into a {@see NexusApiException} carrying the node's own error
 * message, which is what an admin needs to see in the module log.
 */
final class NexusClient
{
    public const USER_AGENT = 'NexusPanel-WHMCS/1.0';
    public const DEFAULT_TIMEOUT = 30;
    public const DEFAULT_HTTP_PORT = 3000;
    public const DEFAULT_HTTPS_PORT = 443;

    private string $baseUrl;
    private string $apiKey;
    private int $timeout;
    /** @var callable(string,string,array<string>,?string):array{0:int,1:string} */
    private $transport;

    /**
     * @param callable|null $transport Replacement for curl, for tests:
     *        fn(string $method, string $url, string[] $headers, ?string $body): [int $status, string $body]
     */
    public function __construct(string $baseUrl, string $apiKey, ?callable $transport = null, int $timeout = self::DEFAULT_TIMEOUT)
    {
        $this->baseUrl = rtrim($baseUrl, '/');
        $this->apiKey = $apiKey;
        $this->timeout = max(1, $timeout);
        $this->transport = $transport ?? [$this, 'curlTransport'];
    }

    /**
     * Build a client from the module parameters WHMCS passes to every function.
     *
     * @param array<string,mixed> $params
     */
    public static function fromParams(array $params, ?callable $transport = null): self
    {
        $apiKey = trim((string) ($params['serveraccesshash'] ?? ''));
        if ($apiKey === '') {
            $apiKey = trim((string) ($params['serverpassword'] ?? ''));
        }
        if ($apiKey === '') {
            throw new NexusApiException(
                'No node API key configured: put the node\'s API key (AUTH_API_KEYS) in the server\'s Access Hash field'
            );
        }

        return new self(self::baseUrlFromParams($params), $apiKey, $transport);
    }

    /**
     * The panel's public base URL as WHMCS's server record describes it.
     *
     * @param array<string,mixed> $params
     */
    public static function baseUrlFromParams(array $params): string
    {
        $secure = self::truthy($params['serversecure'] ?? '');
        $host = trim((string) ($params['serverhostname'] ?? ''));
        if ($host === '') {
            $host = trim((string) ($params['serverip'] ?? ''));
        }
        if ($host === '') {
            throw new NexusApiException('The WHMCS server record has neither a hostname nor an IP address');
        }
        if (!preg_match('/^[A-Za-z0-9.\-:\[\]]+$/', $host)) {
            throw new NexusApiException('The WHMCS server hostname contains characters that cannot be part of a URL');
        }

        $port = (int) ($params['serverport'] ?? 0);
        $defaultPort = $secure ? self::DEFAULT_HTTPS_PORT : self::DEFAULT_HTTP_PORT;
        if ($port <= 0) {
            $port = $defaultPort;
        }
        $standard = $secure ? 443 : 80;

        $url = ($secure ? 'https' : 'http') . '://' . $host;
        if ($port !== $standard) {
            $url .= ':' . $port;
        }

        return $url;
    }

    public static function truthy($value): bool
    {
        if (is_bool($value)) {
            return $value;
        }
        $v = strtolower(trim((string) $value));

        return in_array($v, ['1', 'on', 'true', 'yes'], true);
    }

    public function baseUrl(): string
    {
        return $this->baseUrl;
    }

    // ── Node ──────────────────────────────────────────────────────────

    /** @return array<string,mixed> */
    public function nodeInfo(): array
    {
        return $this->request('GET', '/api/v1/node/info');
    }

    /**
     * The blueprints this node ships.
     *
     * @return array<int,array{id:string,name:string,game:string}>
     */
    public function blueprints(): array
    {
        $list = $this->request('GET', '/api/v1/blueprints');
        $out = [];
        foreach ($list as $bp) {
            if (is_array($bp) && isset($bp['id'])) {
                $out[] = [
                    'id' => (string) $bp['id'],
                    'name' => (string) ($bp['name'] ?? $bp['id']),
                    'game' => (string) ($bp['game'] ?? ''),
                ];
            }
        }

        return $out;
    }

    // ── Provisioning ──────────────────────────────────────────────────

    /**
     * @param array<string,mixed> $payload
     * @return array<string,mixed>
     */
    public function provision(array $payload): array
    {
        return $this->request('POST', '/api/v1/provision/servers', $payload);
    }

    /** @return array<string,mixed> */
    public function server(string $serverId): array
    {
        return $this->request('GET', '/api/v1/provision/servers/' . rawurlencode($serverId));
    }

    /** @return array<string,mixed>|null */
    public function findByExternalId(string $externalId): ?array
    {
        $found = $this->request('GET', '/api/v1/provision/servers', null, ['external_id' => $externalId]);

        return $found[0] ?? null;
    }

    /**
     * @param array<string,mixed> $payload
     * @return array<string,mixed>
     */
    public function changePackage(string $serverId, array $payload): array
    {
        return $this->request('POST', '/api/v1/provision/servers/' . rawurlencode($serverId) . '/package', $payload);
    }

    /** @return array<string,mixed> */
    public function terminate(string $serverId): array
    {
        return $this->request('DELETE', '/api/v1/provision/servers/' . rawurlencode($serverId));
    }

    /** @return array<string,mixed> */
    public function usage(string $serverId): array
    {
        return $this->request('GET', '/api/v1/provision/servers/' . rawurlencode($serverId) . '/usage');
    }

    /**
     * Mint a one-time sign-in link for a customer.
     *
     * @return array{token:string,path:string,expires_in_secs:int}
     */
    public function ssoToken(string $serverId, string $subject, ?int $sessionTtlSecs = null): array
    {
        $payload = ['server_id' => $serverId, 'subject' => $subject];
        if ($sessionTtlSecs !== null && $sessionTtlSecs > 0) {
            $payload['session_ttl_secs'] = $sessionTtlSecs;
        }

        return $this->request('POST', '/api/v1/provision/sso', $payload);
    }

    /** The absolute URL a customer's browser follows to sign in. */
    public function ssoUrl(string $path): string
    {
        return $this->baseUrl . $path;
    }

    // ── Power / lifecycle ─────────────────────────────────────────────

    public function suspend(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/suspend');
    }

    public function unsuspend(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/unsuspend');
    }

    public function start(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/start');
    }

    public function stop(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/stop', []);
    }

    public function restart(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/restart');
    }

    /** Re-run the server's game-file install (its repair path). */
    public function reinstall(string $serverId): void
    {
        $this->request('POST', '/api/v1/containers/' . rawurlencode($serverId) . '/install');
    }

    // ── Transport ─────────────────────────────────────────────────────

    /**
     * @param array<string,mixed>|null $body
     * @param array<string,string> $query
     * @return array<string,mixed>
     */
    private function request(string $method, string $path, ?array $body = null, array $query = []): array
    {
        $url = $this->baseUrl . $path;
        if ($query !== []) {
            $url .= '?' . http_build_query($query);
        }

        $headers = [
            'Accept: application/json',
            'User-Agent: ' . self::USER_AGENT,
            'X-Api-Key: ' . $this->apiKey,
        ];
        $encoded = null;
        if ($body !== null) {
            $encoded = json_encode($body, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
            $headers[] = 'Content-Type: application/json';
        }

        try {
            [$status, $raw] = ($this->transport)($method, $url, $headers, $encoded);
        } catch (NexusApiException $e) {
            throw $e;
        } catch (\Throwable $e) {
            throw new NexusApiException('Could not reach the Nexus node at ' . $this->baseUrl . ': ' . $e->getMessage(), 0, $e);
        }

        $decoded = null;
        if ($raw !== '') {
            $decoded = json_decode($raw, true);
        }

        if ($status < 200 || $status >= 300) {
            $message = is_array($decoded) && isset($decoded['error'])
                ? (string) $decoded['error']
                : 'HTTP ' . $status . ' from the Nexus node';
            throw new NexusApiException($message, $status);
        }

        if ($raw === '') {
            return [];
        }
        if (!is_array($decoded)) {
            throw new NexusApiException('The Nexus node returned a response that is not JSON (is the URL pointing at the panel?)', $status);
        }

        return $decoded;
    }

    /**
     * @param string[] $headers
     * @return array{0:int,1:string}
     */
    private function curlTransport(string $method, string $url, array $headers, ?string $body): array
    {
        $ch = curl_init($url);
        if ($ch === false) {
            throw new NexusApiException('curl_init failed');
        }

        curl_setopt_array($ch, [
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_HTTPHEADER => $headers,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_FOLLOWLOCATION => false,
            CURLOPT_CONNECTTIMEOUT => 10,
            CURLOPT_TIMEOUT => $this->timeout,
            CURLOPT_SSL_VERIFYPEER => true,
            CURLOPT_SSL_VERIFYHOST => 2,
            CURLOPT_PROTOCOLS => CURLPROTO_HTTP | CURLPROTO_HTTPS,
        ]);
        if ($body !== null) {
            curl_setopt($ch, CURLOPT_POSTFIELDS, $body);
        }

        $raw = curl_exec($ch);
        if ($raw === false) {
            $error = curl_error($ch);
            curl_close($ch);
            throw new NexusApiException('Could not reach the Nexus node at ' . $this->baseUrl . ': ' . $error);
        }
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        curl_close($ch);

        return [$status, (string) $raw];
    }
}
