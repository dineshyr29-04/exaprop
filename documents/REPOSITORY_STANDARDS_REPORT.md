# Repository standards engineering report

Initial audit date: 22 October 2026. Base: `main` / `origin/main` at `c63747f322a241328dc704046feefae323193536`. Working branch: `chore/repository-standards`, created from the clean, freshly fetched main branch. The initial audit and local validation preceded publication; they did not commit or push changes. The initial fast-forward check reported main already up to date.

The user subsequently authorized six focused commits and pushing this branch. That initial published tip, `93c8293ba4a467b8dcfceecedcc38a97050af73f`, passed [GitHub Actions run 39784417419](https://github.com/dineshyr29-04/exaprop/actions/runs/39784417419). The branch awaits pull request review and merge; no PR was open when rechecked. The original local evidence below is preserved separately from that later publication and hosted verification.

The follow-up refinements were subsequently committed and pushed as `d1a988e` (CI trigger refinement) and `f667fae` (documentation reconciliation). The latest published tip, `f667fae`, still requires its own hosted verification through a pull request or manual workflow dispatch; run 39784417419 verifies only `93c8293`. No remote settings were changed.

## Audit findings

- M3 source, regression tests, and detailed documentation were present on main, but the README still described observability as planned, omitted `--name`, and listed 21 tests. Its HTTP 205 description and roadmap were outdated.
- The README duplicated long protocol and research descriptions while omitting the current observation contract and smoke-test entry points.
- Bug/feature forms and the greeting workflow still used FaultLab branding. Contact links pointed at the wrong repository; the actual ExaProp repository has Discussions disabled.
- The PR template used decorative emojis, a dangling `Closes #`, and broad test checkboxes that did not fit documentation changes.
- Existing CI already covered Linux/Windows Rust checks and both M3 smoke suites. It lacked cancellation of superseded runs, dependency caches, immutable action references, explicit credential-free checkout, and an action-update strategy.
- `CONTRIBUTING.md` omitted the smoke suites, consistent locked builds, and workflow-maintenance guidance. The PowerShell helper already covered Rust checks but not Python smoke tests.
- GitHub's private-vulnerability-reporting API returned `enabled: false`; no alternate private contact was published. The old policy described a conditional route without making that gap clear.
- Both Cargo packages remain version 0.1.0. The toolchain follows `stable`, rather than pinning a compiler release; no tested MSRV or formal security-support matrix is declared.
- The tracked file inventory contained intentional source, fixtures, lockfile, policies, and engineering documentation. Build output, environments, and local evidence were already ignored. No ignore, dependency, license, runtime, or regression-test changes were needed.

## Files changed

| File | Reason |
| --- | --- |
| [README.md](../README.md) | Complete rewrite around current behavior, executable setup, output semantics, limits, documentation, and M4 anomaly-detection roadmap |
| [CONTRIBUTING.md](../CONTRIBUTING.md) | Reproducible build/check/smoke setup, focused PR guidance, privacy, action maintenance, and fork-safe CI expectations |
| [SECURITY.md](../SECURITY.md) | Explicit private-reporting gap, maintainer action, safe report contents, and deployment/version boundaries without invented contacts or SLAs |
| [.github/PULL_REQUEST_TEMPLATE.md](../.github/PULL_REQUEST_TEMPLATE.md) | Summary, motivation, technical changes, validation, compatibility, documentation, reviewer notes, and four relevant checklist items |
| [.github/ISSUE_TEMPLATE/bug_report.yml](../.github/ISSUE_TEMPLATE/bug_report.yml) | ExaProp-specific reproduction, expected/actual behavior, commit/version, OS/architecture, optional Rust details, sanitized command/logs, and context |
| [.github/ISSUE_TEMPLATE/feature_request.yml](../.github/ISSUE_TEMPLATE/feature_request.yml) | Problem, proposed behavior, alternatives, scope/compatibility, and optional contribution interest |
| [.github/ISSUE_TEMPLATE/config.yml](../.github/ISSUE_TEMPLATE/config.yml) | Preserve disabled blank issues; replace broken contacts with the actual security policy |
| [.github/workflows/ci.yml](../.github/workflows/ci.yml) | Concurrency, cache isolation by OS/compiler/lockfile, pinned actions, no persisted checkout credentials, version logging, pip cache, and explicit evidence retention |
| `.github/workflows/welcome.yml` | Remove stale FaultLab greeting automation and its issue/PR write permissions; no M3 verification lived here |
| [.github/dependabot.yml](../.github/dependabot.yml) | New weekly GitHub Actions update proposals for maintaining immutable pins |
| [documents/REPOSITORY_STANDARDS_REPORT.md](REPOSITORY_STANDARDS_REPORT.md) | This audit, file inventory, validation evidence, risks, and proposed PR text |

## README and contributor experience

The first screen now states what the proxy does, that it runs locally without a central ExaProp service, and that anomaly detection is future work. One real CI badge links to the existing workflow; no release or platform badges imply unsupported guarantees. The request flow, quick start, installation, CLI defaults, observation fields, HTTP/resource/security limitations, and documentation links are organized for a new contributor.

The HTTP 205 entry now says zero content with Content-Length: 0; Via, malformed Connection headers, upstream 101 rejection, buffered responses, stage-specific deadlines, bounded draining, privacy limits, and possible telemetry loss match source. Detailed measurement and privacy rules remain in the observation contract. Historical reports are linked as historical evidence rather than current platform certification.

Contribution instructions distinguish Rust checks from Python smoke tests and give Windows and Unix environment commands. PR validation must record actual results and explain skips; documentation changes do not require artificial unit tests or screenshots. Issue forms use existing repository labels (`bug`, `enhancement`) and request synthetic, sanitized reproductions rather than private traffic.

## CI changes and verification coverage

The existing two-OS matrix, 15-minute timeout, manual dispatch, formatting, locked compilation/tests/Clippy/release/doc builds, strict documentation warnings, full FastAPI timeout campaign, HTTP protocol review, and always-attempted smoke artifact upload are retained. Automatic runs now cover pushes to `main` and all pull requests, including forks. Feature-branch push runs are omitted to avoid duplicating the full matrix for internal PRs; before a PR exists, use local checks or manual dispatch.

New concurrency cancels superseded runs for the same PR or branch. The token remains `contents: read`; checkout no longer persists credentials. No workflow executes untrusted PR code under `pull_request_target`. No project secret, hosted ExaProp service, publishing step, or deployment step is introduced. Removing the greeting workflow removes its privileged metadata-writing job as well.

Cargo caches include the OS, actual compiler identity, Cargo.lock, and toolchain file in their key; pip uses the pinned smoke requirements. Cache hits still run every check. Artifact retention is seven days. A cancelled run may not finish evidence output/upload. These are expected maintenance benefits, not a measured CI speed claim.

Action versions and full SHAs were resolved through the official repositories' GitHub APIs and checked against their action metadata and runner compatibility notes:

| Action | Verified pin |
| --- | --- |
| [actions/checkout v7.0.1](https://github.com/actions/checkout/tree/3d3c42e5aac5ba805825da76410c181273ba90b1) | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| [actions/setup-python v7.1.0](https://github.com/actions/setup-python/tree/6d4fd22b20465ccc0f7354476588e8df7c9ca0e0) | `6d4fd22b20465ccc0f7354476588e8df7c9ca0e0` |
| [actions/upload-artifact v7.0.2](https://github.com/actions/upload-artifact/tree/cf430e030ddbb5b0abf93d22962f4752f3646cd9) | `cf430e030ddbb5b0abf93d22962f4752f3646cd9` |
| [actions/cache v6.1.0](https://github.com/actions/cache/tree/55cc8345863c7cc4c66a329aec7e433d2d1c52a9) | `55cc8345863c7cc4c66a329aec7e433d2d1c52a9` |

These Node 24 actions require current runners; the workflow uses GitHub-hosted latest Linux/Windows runners. Self-hosted runners and GitHub Enterprise Server compatibility are not claimed. Dependabot will propose weekly updates after the configuration is merged; maintainers still need to review version comments, release notes, and CI.

## Original local validation results

Local environment: Windows x86-64 GNU Rust 1.99.1, Python 3.12.14. The existing `scripts/check.ps1` was executed with the installed Rust tools on PATH and `CARGO_INCREMENTAL=0`. It ran these commands without weakening assertions or warnings:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS: 54 core + 3 CLI tests, zero failures; zero doctests |
| `cargo build --workspace --locked` | PASS, additional debug build |
| `cargo build --workspace --release --locked` | PASS |
| `cargo doc --workspace --no-deps --locked` | PASS with RUSTDOCFLAGS=-D warnings |
| `.venv/Scripts/python.exe scripts/smoke_fastapi.py --include-timeouts` | PASS: 37 checked requests, 38 observations, complete normal drain; broken-stdout forwarding and measurable drops retained; 35.828 seconds |
| `.venv/Scripts/python.exe scripts/smoke_http_review.py` | PASS: 16 protocol requests, 17 observations, Via chaining, malformed headers, upstream 101, HTTP 205 framing, connection reuse, echo backend outage/restart, and three stderr scenarios |
| `tmp/repository-standards/tools/actionlint/actionlint.exe -color` | PASS using actionlint 1.7.12; no workflow diagnostics |
| `.venv/Scripts/python.exe tmp/repository-standards/validate_metadata.py` | PASS: five YAML files, CI invariants/coverage/pins, issue-form structure/IDs/labels, contact configuration, 83 relative Markdown targets/heading anchors across 11 documents, and unchanged protected source paths |
| `git diff --check` | PASS |
| `git diff --exit-code origin/main -- core cli examples scripts Cargo.toml Cargo.lock rust-toolchain.toml LICENSE .gitignore CODE_OF_CONDUCT.md` | PASS: no changes |

The HTTP stderr checks again completed 600/600 requests while diagnostics were unread, then counted 219 full-queue drops after drainage resumed. Another 600/600 completed with stderr blocked through bounded unsuccessful shutdown; ten requests completed with stderr closed. The smoke suites themselves were not edited.

Local evidence remains ignored: `tmp/repository-standards/rust-checks.log`, the metadata-validation script, `tmp/fastapi-smoke/20261022-161834-52724/`, and `tmp/http-review/20261022-161835-59900/`. The temporary actionlint ZIP was checked against its published SHA-256 list before extraction. The YAML 1.2 parser was installed only under ignored temporary tooling; no project or smoke dependency file changed.

No requested local check was skipped or failed in the initial audit. Hosted CI had not yet run at that stage; it subsequently passed for published commit `93c8293` as recorded above. The GitHub issue-form UI and actual Dependabot PR creation were not exercised. Static validation does not substitute for those hosted checks.

## Final refinement verification

The follow-up fetch confirmed that the clean starting branch matched `origin/chore/repository-standards` and included current `origin/main` at `c63747f`. GitHub reported both `Verify (ubuntu-latest)` and `Verify (windows-latest)` successful for published commit `93c8293`. Private vulnerability reporting was rechecked and still returned `enabled: false`; the existing SECURITY.md warning and required maintainer action remain accurate. No PR was open.

Only three files changed in the published refinement: `.github/workflows/ci.yml` restricts push events to main; CONTRIBUTING.md explains PR/manual verification for feature branches; this report separates the initial audit, later authorized publication, and commit-specific hosted results. The original evidence above is unchanged. The other reviewed documentation and templates required no correction.

Final local checks passed again: every Rust command listed above through `scripts/check.ps1` (57 tests, strict Clippy/rustdoc, debug/release builds); actionlint 1.7.12; YAML 1.2 and issue-form validation; 83 relative Markdown links/anchors; and `git diff --check`. The metadata validator additionally confirms that the complete verification jobs, permissions, and concurrency configuration are identical to the published workflow. Main push, unfiltered PRs including forks, and manual dispatch remain enabled. No runtime or M3 regression source changed against either HEAD or origin/main.

Both full smoke suites passed again. FastAPI checked 37 requests and 38 observations in 36.203 seconds, including both deliberate timeout paths and the broken-stdout scenario. HTTP review passed 16 protocol requests, Via/205/connection-reuse assertions, the echo backend, and all stderr modes: 600 completed requests with drainage resumed and 219 diagnostic drops, 600 with stderr blocked through bounded shutdown, and ten with stderr closed. New ignored evidence: `tmp/repository-standards/refinement-rust-checks.log`, `tmp/fastapi-smoke/20261022-225107-50040/`, and `tmp/http-review/20261022-225105-28456/`.

The scheduling change avoids a second matrix for internal PR updates; it is not a benchmark or a claim that an individual test run becomes faster. No test, platform, timeout case, or assertion was removed. These refinements are published through `f667fae` and await their own hosted verification, PR review, and merge.

## Remaining risks and maintainer decisions

1. Private vulnerability reporting is disabled and there is no published fallback contact. A maintainer must enable it or supply a verified private channel and update the dated policy statement. This is explicitly documented, not silently treated as solved. No repository settings were changed.
2. Hosted CI passed for `93c8293`, including Linux and Windows verification. Latest published commit `f667fae` still requires its own hosted run through a PR or manual workflow dispatch. macOS and actual external-fork PR execution are not claimed.
3. Stable Rust and hosted runner images still move over time. An exact compiler pin, tested MSRV, formal supported-version policy, and branch-protection rules remain maintainer choices; none was invented here.
4. Existing M3 limitations remain: plain HTTP, buffered bodies, finite best-effort telemetry, heuristic privacy, and no anomaly detector or fault injection. This maintenance pass is not a new runtime security audit or production-readiness certification.

## Final diff review and merge readiness

All 11 changed/created/deleted files were reviewed. Rust source, regression tests, Python fixtures/harnesses, manifests, Cargo.lock, toolchain selection, ignore rules, Code of Conduct, and Apache license are unchanged. M3 verification coverage is retained; only the unrelated stale greeting workflow is removed. No M4 behavior or deployment machinery was introduced. There are no generated files or credentials in the intended change set.

**The branch is published through `f667fae` and awaits hosted verification of that commit, pull request review, and merge.** Local validation passed; GitHub Actions run 39784417419 passed only for `93c8293`. Merge also requires a maintainer decision on the documented private-reporting gap. The initial audit did not perform the later authorized commits or pushes; no merge or settings change was performed during the maintenance work.

## Original commit proposal and current pull request draft

Original conventional commit proposal (publication subsequently used six focused commits):

```text
chore(repo): align documentation, CI, and contributor workflows after M3
```

PR title:

```text
Align repository standards and contributor workflows with implemented M3
```

Proposed PR description:

> Main's README and contributor templates still describe pre-M3 behavior and retain FaultLab links. This change documents the implemented local HTTP proxy and observation contract, identifies M4 anomaly detection as planned, and replaces stale PR/issue guidance with reproducible, privacy-conscious workflows.
>
> CI retains every M3 Rust and real-backend smoke check on Linux and Windows while adding immutable action pins, credential-free checkout, concurrency, Cargo/pip caching, and seven-day smoke evidence retention. Dependabot proposes action updates weekly. The stale greeting workflow is removed with its unnecessary write permissions.
>
> Validation passed locally on Windows GNU: formatting, locked check/test/debug/release builds, strict all-target/all-feature Clippy, strict rustdoc, 57 Rust tests, full FastAPI smoke, HTTP/blocked-output smoke, actionlint, YAML 1.2/issue-form checks, Markdown links/anchors, and diff checks. Runtime source, test coverage, dependencies, and license are unchanged.
>
> Reviewer attention: private vulnerability reporting is disabled and no fallback private contact is published. SECURITY.md flags the maintainer action rather than inventing a contact or support promise. Run 39784417419 passed for `93c8293`; latest published commit `f667fae` needs its own hosted verification through a PR or manual dispatch. The branch awaits PR review and merge.
