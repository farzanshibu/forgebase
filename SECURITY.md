# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub's **Report a vulnerability** button on the
repository's Security tab (private vulnerability reporting). Include:

- the ForgeBase version (`forgebase --version`) and how it is deployed,
- what an attacker can do, and the steps or a test that shows it,
- any logs, with secrets removed.

We aim to acknowledge a report within 3 working days and to agree a fix and a
disclosure date with you. We credit reporters who want to be credited.

## Supported versions

ForgeBase is pre-1.0. Security fixes land on the latest release only.

## How ForgeBase is secured

The threat model, the controls and the bypass test suite are described in
[docs/security.md](docs/security.md). Deployment hardening (TLS, the reverse
proxy, `ops_access`, `trust_forwarded_for`) is in
[docs/deployment.md](docs/deployment.md).
