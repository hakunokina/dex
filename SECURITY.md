# Security policy

Report vulnerabilities privately through this repository's **Security → Advisories → Report a vulnerability** page on GitHub. Include the affected commit/version, OS and CLI versions, reproduction steps, impact and a minimal proof of concept. Remove real credentials and personal data.

Do not post exploit details or credentials in public issues. If the private reporting button is absent, open an issue asking the maintainer to enable private vulnerability reporting, without disclosing the vulnerability. See [GitHub's private reporting instructions](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately).

Support is best effort; there is no promised response time, security SLA or guaranteed fix. Coordinate disclosure with the maintainer after submitting the private report.

Dex launches agents with broad OS-user privileges by default. Its hook policies and review gates are application controls, not an OS sandbox. See the [README security section](README.md#security) and [network behavior](docs/network.md).

## Trust model

The publication tools in `scripts/yayin/` are **not a sandbox** and run only trusted project sources. Their Compose gate limits host access but cannot isolate malicious builds or code, existing Docker resources, or filesystem races. Use a separate VM or a dedicated rootless Docker daemon for untrusted projects; membership in a rootful daemon's Docker group grants host administrator access.
