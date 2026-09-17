# Security policy

## Supported versions

Fixes land in the newest minor release line only.

| Version | Supported |
| --- | --- |
| 2.1.x | yes |
| 2.0.x | no — upgrade to 2.1 |
| 1.x | no |

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub:
[Security → Report a vulnerability](https://github.com/AxDSan/kiln/security/advisories/new).
Include the Kiln version (`kiln --version`), the platform, and a program or steps
that show the problem.

You can expect an acknowledgement within a week. Once a fix is ready it ships as
a patch release, and the advisory is published with it, crediting you unless you
ask not to be.

## What counts

- The compiler or runtime producing a binary that is unsafe in a way the source
  did not ask for: memory corruption from safe Kiln code, a bounds check that
  is missing, a collector that frees a reachable value.
- The runtime's networking, `http`, `https` or `db` code mishandling untrusted
  input.
- The language server, debug adapter or Studio doing something harmful when
  opening an untrusted project.
- A release bundle that is not what its checksum and the tagged source say.

Out of scope: what a program does with `[Dll]` calls, raw pointers or `ptr_*`
commands (these reach C deliberately), and `https` requests made without the
vendored mbedTLS, which fail rather than downgrade by design.
