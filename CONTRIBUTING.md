# Contributing to FaultLab

Thank you for your interest in contributing to **FaultLab**! 🚀

FaultLab is an open-source resilience modeling and incident investigation engine for distributed systems. We welcome contributions from developers, site reliability engineers (SREs), system architects, and technical writers of all backgrounds and experience levels.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [How Can I Contribute?](#how-can-i-contribute)
  - [Reporting Bugs](#reporting-bugs)
  - [Suggesting Features & Enhancements](#suggesting-features--enhancements)
  - [Improving Documentation](#improving-documentation)
  - [Submitting Code Changes](#submitting-code-changes)
- [Development Workflow](#development-workflow)
  - [1. Fork & Clone](#1-fork--clone)
  - [2. Branching Convention](#2-branching-convention)
  - [3. Commit Message Guidelines](#3-commit-message-guidelines)
  - [4. Creating a Pull Request](#4-creating-a-pull-request)
- [Review Process & Expectations](#review-process--expectations)
- [Community & Questions](#community--questions)

---

## Code of Conduct

This project and everyone participating in it is governed by the [FaultLab Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to the project maintainers.

---

## How Can I Contribute?

### Reporting Bugs

Before creating a bug report, please check existing [GitHub Issues](https://github.com/dineshyr29-04/faultlab/issues) to confirm the bug hasn't already been reported.

When filing a bug report:
- Use the **Bug Report** issue template.
- Describe the expected behavior vs. the actual behavior.
- Include minimal, reproducible steps or code snippets.
- Specify your operating system, runtime/compiler versions, and relevant logs.

### Suggesting Features & Enhancements

Feature requests are always welcome!
- Use the **Feature Request** issue template.
- Explain the problem or use case you are trying to solve.
- Describe your proposed solution or API design.
- Consider backwards compatibility and potential edge cases.

### Improving Documentation

Documentation improvements are as valuable as code changes:
- Fixing typos, grammatical errors, or broken links.
- Adding architectural diagrams or practical examples.
- Clarifying installation, deployment, and configuration steps.

---

## Development Workflow

### 1. Fork & Clone

1. Fork the repository on GitHub: `https://github.com/dineshyr29-04/faultlab`
2. Clone your fork locally:
   ```bash
   git clone https://github.com/<your-username>/faultlab.git
   cd faultlab
   ```
3. Add the upstream remote:
   ```bash
   git remote add upstream https://github.com/dineshyr29-04/faultlab.git
   ```

### 2. Branching Convention

Always create a dedicated feature branch from `main`:

```bash
git checkout main
git pull upstream main
git checkout -b <type>/<short-description>
```

Branch naming prefixes:
- `feat/` — New feature or capability
- `fix/` — Bug fix or error resolution
- `docs/` — Documentation updates
- `perf/` — Performance optimization
- `refactor/` — Code refactoring with no behavioral change
- `test/` — Adding or updating test suites
- `chore/` — Build system, CI, dependencies, or tooling

*Example: `git checkout -b feat/chaos-latency-injector`*

### 3. Commit Message Guidelines

We follow the [Conventional Commits](https://www.conventionalcommits.org/) specification:

```
<type>(<optional scope>): <description>

[optional body]

[optional footer(s)]
```

#### Allowed Types:
- `feat`: A new feature
- `fix`: A bug fix
- `docs`: Documentation only changes
- `style`: Formatting changes that do not affect code logic
- `refactor`: Code change that neither fixes a bug nor adds a feature
- `perf`: Code change that improves performance
- `test`: Adding missing tests or correcting existing tests
- `chore`: Changes to build process, CI workflows, or auxiliary tools

*Example:*
```
feat(engine): add probabilistic network partition simulation

Implements network partition matrix modeling with asymmetric packet drop support.
Resolves #42
```

### 4. Creating a Pull Request

1. Push your branch to your fork:
   ```bash
   git push origin <type>/<short-description>
   ```
2. Open a Pull Request against the `main` branch of `dineshyr29-04/faultlab`.
3. Complete the provided **Pull Request Template**:
   - Provide a clear title following Conventional Commits.
   - Reference any related issue(s) using GitHub keywords (e.g. `Closes #12`).
   - Describe what changed and why.
   - Include any verification/test commands executed.
4. Ensure all CI checks and linters pass.

---

## Review Process & Expectations

- Every Pull Request requires review and approval from at least one maintainer.
- Reviewers may ask questions or suggest refinements. Please keep discussions constructive and collaborative.
- Once approved, maintainers will squash-and-merge your contribution into `main`.

Thank you for helping build **FaultLab**! ⭐
