# Security policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| 1.x     | ✅ the latest release |
| < 1.0   | ❌ |

Fixes ship as a new 1.x release. Only the latest release is patched, so upgrade to it first.

## Reporting a vulnerability

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/dlepaux/seerr-mcp/security/advisories/new),
not in a public issue.

Include:

- what you found and its impact;
- the steps to reproduce it;
- the version (`seerr-mcp --version`);
- a suggested fix, if you have one.

You should get a first answer within 7 days. The advisory is published, with credit to you if you
want it, once the fixed release is out.

## Scope

The security model is in the [readme](readme.md#security-model). Of most interest:

- acting as another person, or as the Seerr admin;
- filing a request without the person's confirmation;
- getting past the `/mcp` bearer or the `/notify` secret;
- a token, key or secret reaching the logs.

Out of scope: anything that needs the Seerr admin API key or the identities file, since whoever
holds those already controls the deployment.
