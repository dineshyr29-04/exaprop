# Security policy

ExaProp is an early-stage HTTP proxy and observability project. Security concerns include unsafe HTTP handling, unintended network exposure, sensitive output, dependency vulnerabilities, and code execution or privilege escalation.

## Private reporting

**Do not report suspected vulnerabilities through public issues, pull requests, discussions, or CI logs.** Do not upload credentials, authorization headers, private URLs, traffic captures, or exploit details publicly.

GitHub private vulnerability reporting is currently disabled for this repository (verified 22 October 2026). No alternate private security contact is published in the repository. This is a reporting-channel gap, not an invitation to disclose publicly. If you already have a verified private channel to a maintainer, use it; otherwise withhold sensitive details until a private route is established.

**Maintainer action required:** enable private vulnerability reporting under the repository's security settings, or publish a verified private contact here. No response-time commitment or contact address is implied by this policy.

Once private reporting is enabled, open the repository's **Security → Advisories → Report a vulnerability** flow and submit the report privately. The current [advisories page](https://github.com/dineshyr29-04/exaprop/security/advisories) is not itself proof that private reporting is enabled.

A useful private report includes:

- Affected version or commit, platform, and component.
- Minimal reproduction using synthetic data and systems you control.
- Expected and actual behavior, impact, and any known mitigation.
- Sanitized logs sufficient to reproduce the issue without exposing unrelated data.

Avoid accessing, modifying, or deleting data belonging to others. Coordinate public disclosure with maintainers after investigation and remediation; no response SLA is currently published.

## Versions and deployment boundaries

No formal supported-version matrix or security-support lifetime is published. Development interfaces and behavior may change. Do not infer a maintenance promise from a version number or roadmap milestone.

Current ExaProp traffic is plain HTTP. The proxy provides no built-in authentication or public-edge hardening. Observation filtering is heuristic: visible paths, query-name spelling, and unrecognized query values can contain secrets. Keep both observations and diagnostics access-controlled and review them before sharing. Bounded queues may drop output and are not an audit log.

Use ExaProp only with systems you own or are authorized to test. Future anomaly detection, fault injection, or traffic manipulation proposals do not change these boundaries. See the [README limitations](README.md#http-behavior-and-limitations) and [observation privacy contract](documents/M3_OBSERVABILITY.md#privacy-policy).
