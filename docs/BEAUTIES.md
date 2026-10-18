# Private workstation packages

Gitea's private `beauties/packages` publisher monitors this repository's GitHub
`main` every five minutes. It pins a source commit, validates it, and publishes
signed Fedora RPMs plus a checksum-pinned native Homebrew source formula.
Existing tagged GitHub releases remain separate.

The package embeds its source commit and increasing main-build revision in
`--version`; `beauties-build.json` records source and recipe provenance. Routine
Fedora updates and the managed daily macOS Beauties updater install new builds.
Configuration, backend trust, databases and user sessions stay user-owned.

For operation, acceptance and rollback, see `homelab/docs/beauties.md` in infra.
