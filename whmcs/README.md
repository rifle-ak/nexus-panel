# WHMCS provisioning module

`modules/servers/nexuspanel` is a WHMCS provisioning module for Nexus Panel:
it creates, suspends, upgrades and terminates game servers on a Nexus node as
orders come and go, signs customers in to their server with one click, and
reports disk usage.

Install by copying the directory into your WHMCS `modules/servers/`. Full
setup — node configuration, the WHMCS server record, product settings,
configurable options — is in [docs/WHMCS.md](../docs/WHMCS.md).

Tests run on plain PHP with no WHMCS install:

```bash
php tests/run.php
```
