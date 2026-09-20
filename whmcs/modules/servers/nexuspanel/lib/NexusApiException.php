<?php

declare(strict_types=1);

namespace WHMCS\Module\Server\NexusPanel;

/**
 * A request to the Nexus node failed: the node refused it, could not be
 * reached, or answered with something that was not JSON.
 */
final class NexusApiException extends \RuntimeException
{
    /** HTTP status of the node's answer, or 0 when it never answered. */
    private int $status;

    public function __construct(string $message, int $status = 0, ?\Throwable $previous = null)
    {
        parent::__construct($message, $status, $previous);
        $this->status = $status;
    }

    public function getStatus(): int
    {
        return $this->status;
    }

    public function isNotFound(): bool
    {
        return $this->status === 404;
    }
}
