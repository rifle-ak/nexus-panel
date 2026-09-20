{* Nexus Panel — client area overview for a provisioned game server *}
<div class="row">
    <div class="col-md-12">
        <div class="alert alert-{if $isRunning}success{elseif $isSuspended}warning{elseif $installState == 'failed' || $status == 'failed'}danger{else}info{/if}" role="alert">
            <strong>{$statusLabel}</strong>
            {if $installing}&mdash; installing game files, this can take a few minutes.{elseif $installState == 'failed'}&mdash; the game files failed to install. Open the panel to see why and retry.{elseif $isSuspended}&mdash; this server is suspended. Settle any outstanding invoice to bring it back.{/if}
        </div>
    </div>
</div>

<div class="row">
    <div class="col-md-6">
        <div class="panel panel-default card">
            <div class="panel-heading card-header"><h3 class="panel-title card-title">Connection</h3></div>
            <div class="panel-body card-body">
                <table class="table table-condensed table-sm" style="margin-bottom:0">
                    <tr><td><strong>Address</strong></td><td><code>{$address|escape}</code></td></tr>
                    {foreach $ports as $p}
                    <tr><td>{$p.name|escape|capitalize}</td><td><code>{$p.port|escape}</code> <small class="text-muted">{$p.protocol|escape}</small></td></tr>
                    {/foreach}
                </table>
            </div>
        </div>
    </div>
    <div class="col-md-6">
        <div class="panel panel-default card">
            <div class="panel-heading card-header"><h3 class="panel-title card-title">Server</h3></div>
            <div class="panel-body card-body">
                <table class="table table-condensed table-sm" style="margin-bottom:0">
                    <tr><td><strong>Name</strong></td><td>{$serverName|escape}</td></tr>
                    <tr><td><strong>Game</strong></td><td>{$game|escape}</td></tr>
                    <tr><td><strong>Memory</strong></td><td>{$memoryGb} GB</td></tr>
                    <tr><td><strong>CPU</strong></td><td>{$cpuCores} cores</td></tr>
                    <tr><td><strong>Disk</strong></td><td>{$diskGb} GB</td></tr>
                    {if $maxPlayers}<tr><td><strong>Player slots</strong></td><td>{$maxPlayers|escape}</td></tr>{/if}
                    <tr><td><strong>Game files</strong></td><td>{$installLabel|escape}</td></tr>
                </table>
            </div>
        </div>
    </div>
</div>

<div class="row">
    <div class="col-md-12 text-center" style="margin-top:1em">
        <a href="{$ssoUrl|escape}" class="btn btn-primary btn-lg" target="_blank" rel="noopener">
            <i class="fas fa-external-link-alt"></i> Open game panel
        </a>
        <p class="text-muted" style="margin-top:0.75em">
            Console, files, backups, mods and schedules live in the panel. Sign-in is automatic.
        </p>
    </div>
</div>
