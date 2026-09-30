# Security Policy

Thanks for taking the time to help keep this project secure!
svir is currently in **0.x** and maintained by a small team, so the security
process is intentionally lightweight.

## Supported Versions

During the **0.x** stage, we generally **support security fixes only for the latest
released version**. Until the first release is out, fixes land on `main`.

- **Supported:** latest `0.x.y`
- **Not supported:** older `0.x.y` versions (no backports)

In practice, security fixes are published as part of the next release.
If a fix is important, we may ship a quick patch release, but we typically
won't maintain intermediate versions once a newer release is available.

## What is in scope

svir sits between an application and a model server. It holds the application's
credentials, and it treats the server's responses and the model's output as untrusted
input. Reports about these are especially welcome:

- A credential that reaches an error, an event, `Debug` or `Display` output, or a log.
- A request sent over plain HTTP to a host that is not loopback without `allow_http()`,
  a redirect that is followed, or TLS validation that can be bypassed.
- A response that makes svir use memory or time without bound, past its limits.
- A request body whose bytes disagree with the length it declared.
- A tool call handed out as executable before the response is complete, or a `Tools`
  handler run with arguments that do not fit its type.

The guarantees svir makes are listed in [docs/architecture.md](docs/architecture.md#6-security).

Out of scope: what a model says, and what it asks a tool to do. svir runs only the
handlers an application registered; what those handlers are allowed to do is for the
application to decide.

## Reporting a Vulnerability

If you believe you've found a security issue, please **do not open a public issue**
right away.

Instead, report it privately:
- Open a **GitHub Security Advisory** (preferred):
  <https://github.com/RomanEmreis/svir/security/advisories/new>, or
- Contact the maintainer via the repository's listed contact channels.

Please include:
- A clear description of the issue
- Impact and severity (if known)
- Steps to reproduce / PoC (if available)
- Affected versions and environment details, including the model server if it matters

Please leave real API keys and private conversations out of the report.

## What to Expect

We aim to:
- acknowledge reports within a reasonable time,
- assess the impact and decide on a fix,
- publish a release containing the fix,
- credit the reporter (if desired).

If the issue is already public or actively exploited, we may prioritize a faster
release.

## Coordinated Disclosure

We appreciate responsible disclosure and will work with you on a reasonable
timeline for releasing details once a fix is available.
