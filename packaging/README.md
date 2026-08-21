# Packaging

## What's automated

Pushing a tag like `v0.1.0` runs [`.github/workflows/release.yml`](../.github/workflows/release.yml), which:

- builds native release binaries for macOS (Intel + Apple Silicon) and Linux (x86_64 + arm64)
- builds a `.deb` and `.rpm` for x86_64 and arm64 (via `cargo-deb` / `cargo-generate-rpm`, configured in [`Cargo.toml`](../Cargo.toml)), each built and install-tested on a runner of its own architecture
- attaches all of it to a GitHub Release

That covers "download a binary" and "download a `.deb`/`.rpm`". It does **not** cover Homebrew, Fedora COPR, or the AUR — those each publish through a repo/account only a human can set up. Below is what's left for each, once the project has a real public repo and at least one tagged release to point at.

## What the metadata already covers

`repository`, the deb `maintainer`/`copyright`, and `license = "MIT"` are all set in `Cargo.toml`, and the license declaration is backed by a `LICENSE` file in the repo root — so `cargo deb` and `cargo generate-rpm` have everything they need without further setup.

## Testing the deb/rpm build locally

```sh
cargo install cargo-deb cargo-generate-rpm
cargo build --release
cargo deb --no-build          # -> target/debian/hoot_*.deb
cargo generate-rpm            # -> target/generate-rpm/hoot-*.rpm
```

## Homebrew (tap)

1. Create a new GitHub repo named `homebrew-hoot` (the `homebrew-` prefix is what makes `brew tap` find it).
2. Add `Formula/hoot.rb` there, pointing at the release tarball URLs the workflow above produces, with each one's `sha256` (from the release's checksums, or `shasum -a 256` on the downloaded tarball).
3. Users install with `brew tap fclaude/hoot && brew install hoot`.

A single formula can cover both macOS binaries with an `on_arm`/`on_intel` block — there's nothing to write until a tagged release exists to point at.

## Fedora (COPR)

1. Create an account at [copr.fedorainfracloud.org](https://copr.fedorainfracloud.org) and a new project (e.g. `fclaude/hoot`).
2. Point it at a `.spec` file (COPR can build from a source tarball + spec, or from the `.rpm` this workflow already produces).
3. Users install with `dnf copr enable fclaude/hoot && dnf install hoot`.

## Arch (AUR)

1. Create an AUR account and register an SSH key at [aur.archlinux.org](https://aur.archlinux.org).
2. Write (or generate with `cargo-aur`) a `PKGBUILD` — most Rust CLIs publish a `hoot-bin` package that just downloads the release tarball, rather than building from source, since it installs instantly.
3. `git push` it to the AUR's git remote for the package.
4. Users install with `yay -S hoot-bin` or `paru -S hoot-bin` (or any other AUR helper) — matching the package name from step 2. A plain `hoot` would be the build-from-source package, which is a second, separate submission.
