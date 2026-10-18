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

## Updating and checking an installed build

Use `sudo dnf upgrade hoot` on Fedora or `brew upgrade beauties/tap/hoot` on
macOS. `hoot --version` reports the full source SHA and main-build revision.
The automatic update jobs run those package-manager updates routinely.

Review and Curate remain usable without an agent backend. Agent installation,
configuration and the per-user backend trust acknowledgement remain explicit;
installing or upgrading the package never grants that trust.
