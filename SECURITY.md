# Security policy

ExaProp is an early-stage HTTP proxy and observability project. Security concerns include unsafe HTTP handling, unintended network exposure, sensitive output, dependency vulnerabilities, and code execution or privilege escalation.

## Private reporting

**Do not report suspected vulnerabilities through public issues, pull requests, discussions, or CI logs.** Do not upload credentials, authorization headers, private URLs, traffic captures, or exploit details publicly.

GitHub private vulnerability reporting is enabled for this repository (enabled with maintainer approval and verified through the repository API on 21 November 2026).

Open **Security → Advisories → Report a vulnerability**, or use the [private reporting form](https://github.com/dineshyr29-04/exaprop/security/advisories/new), and submit the report privately. If that flow is unavailable, do not fall back to a public issue; use an already verified private maintainer channel or withhold sensitive details until private reporting is restored. No security email address or response-time commitment is published or implied.

A useful private report includes:

- Affected version or commit, platform, and component.
- Minimal reproduction using synthetic data and systems you control.
- Expected and actual behavior, impact, and any known mitigation.
- Sanitized logs sufficient to reproduce the issue without exposing unrelated data.

Avoid accessing, modifying, or deleting data belonging to others. Coordinate public disclosure with maintainers after investigation and remediation; no response SLA is currently published.

## Versions and deployment boundaries

No formal supported-version matrix or security-support lifetime is published. Development interfaces and behavior may change. Do not infer a maintenance promise from a version number or roadmap milestone.

Current ExaProp traffic is plain HTTP. The proxy provides no built-in authentication or public-edge hardening. Observation filtering is heuristic: visible paths, query-name spelling, and unrecognized query values can contain secrets. Keep both observations and diagnostics access-controlled and review them before sharing. Bounded queues may drop output and are not an audit log.

Use ExaProp only with systems you own or are authorized to test. Anomaly detection and future fault-injection or traffic-manipulation proposals do not change these boundaries. See the [current limitations](documents/EXAPROP_USAGE.md#current-limits) and [observation privacy contract](documents/M3_OBSERVABILITY.md#privacy-policy).
