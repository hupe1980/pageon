# Security policy

## Reporting a vulnerability

Please report a suspected vulnerability **privately** through GitHub's private vulnerability
reporting: <https://github.com/hupe1980/pageon/security/advisories/new>.

Do not open a public issue, pull request or discussion. Include the pageon version, the OS,
whether the page ran in the sandbox (the default) or with `--no-sandbox`, and the smallest page
and command that show the problem.

## Scope

What pageon is meant to defend against, what it trusts and where it stops is described in the
[threat model](THREAT_MODEL.md), together with every known threat class and its status. Reports
about classes listed there as open are still welcome when they show a new variant or a worse
impact.

Sandbox escapes that come from procage itself belong to
[procage's security policy](https://github.com/hupe1980/procage/blob/main/SECURITY.md).

## Supported versions

pageon is pre-release. Only the latest version gets fixes.

## What to expect

You can expect an acknowledgement within a week. A confirmed vulnerability gets an entry in the
threat model and a regression test.
