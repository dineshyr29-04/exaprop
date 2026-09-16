# Security Policy

## Scope

ExaProp is an early-stage open-source systems and research project.

Security reports are especially important for issues involving:

* arbitrary code execution
* privilege escalation
* unintended network exposure
* sensitive information disclosure
* unsafe request handling
* vulnerabilities in the proxy or observation components
* vulnerabilities introduced through project dependencies

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub Issues.**

Use GitHub's private vulnerability reporting / Security Advisories mechanism when it is available for the repository.

When reporting a vulnerability, please include:

* a clear description of the issue
* affected component or version
* reproduction steps or proof of concept
* expected behavior
* actual behavior
* potential security impact
* any known mitigation, if available

Please provide enough information for the issue to be reproduced and investigated.

## Supported Versions

ExaProp is currently under active development.

Until the project establishes a formal release-support policy, security fixes will be handled based on the affected release and the severity of the issue.

Development versions may change without notice.

## Responsible Disclosure

Please allow the maintainers reasonable time to investigate and address a reported vulnerability before publicly disclosing technical details.

Security researchers are encouraged to avoid accessing, modifying, or deleting data that does not belong to them while demonstrating a vulnerability.

## Experimental Features

Some future ExaProp features may involve fault injection, traffic manipulation, or other potentially disruptive experiments.

Such features must only be used against systems that you own or are explicitly authorized to test.

Never use ExaProp's experimental capabilities against production or third-party systems without explicit authorization.
