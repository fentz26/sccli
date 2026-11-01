# Security

## Reporting a vulnerability

Email contact@fentz.dev. Do not open a public issue.

Include the sccli version (`scc --version`), your operating system, and the steps to
reproduce the problem. Never send your `oauth_token`.

Only the latest release receives security fixes.

## Your SoundCloud token

`scc login` stores your `oauth_token` in `config.json` in the settings folder. On Linux and
macOS the file is readable only by your user. Treat it like a password, and run `scc logout`
to remove it.

## Verifying a release

Release archives come with [SLSA Build Level 3](https://slsa.dev/spec/v1.0/levels) provenance,
which proves that an archive was built by this repository's release workflow from a specific
tag. See [Verify a release](README.md#verify-a-release).
