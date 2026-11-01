# Verify a release

You do not need this to use sccli. It is for people who want proof that a download was built by
this project and not changed afterwards.

Each release includes [SLSA Build Level 3](https://slsa.dev/spec/v1.0/levels) provenance, a signed
record of which repository, tag and workflow produced each archive. The release workflow checks
every archive against it before a release is created.

To check one yourself, install
[slsa-verifier](https://github.com/slsa-framework/slsa-verifier), download an archive and
`sccli.intoto.jsonl` from [Releases](https://github.com/fentz26/sccli/releases), then run:

```sh
slsa-verifier verify-artifact sccli-26.01-x86_64-unknown-linux-musl.tar.gz \
  --provenance-path sccli.intoto.jsonl \
  --source-uri github.com/fentz26/sccli \
  --source-tag v26.01
```

`PASSED` means the archive is the one that workflow built from that tag. Any other result means
you should not trust it. Please report it to contact@fentz.dev.
