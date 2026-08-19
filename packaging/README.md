# Packaging

## What's automated

Pushing a tag like `v0.1.0` runs [`.github/workflows/release.yml`](../.github/workflows/release.yml), which:

- builds native release binaries for macOS (Intel + Apple Silicon) and Linux (x86_64 + arm64)
- builds a `.deb` and `.rpm` (x86_64 only, via `cargo-deb` / `cargo-generate-rpm`, configured in [`Cargo.toml`](../Cargo.toml))
- attaches all of it to a GitHub Release

That covers "download a binary" and "download a `.deb`/`.rpm`". It does **not** cover Homebrew, Fedora COPR, or the AUR — those each publish through a repo/account only a human can set up. Below is what's left for each, once the project has a real public repo and at least one tagged release to point at.

## Before any of this works

- `repository` and the deb `maintainer`/`copyright` in `Cargo.toml` are filled in (`fclaude/steer`, `Francisco Claude-Faust <fclaude@recoded.cl>`), and the `license = "MIT"` declaration is backed by a `LICENSE` file in the repo root.

## Testing the deb/rpm build locally

```sh
cargo install cargo-deb cargo-generate-rpm
cargo build --release
cargo deb --no-build          # -> target/debian/steer_*.deb
cargo generate-rpm            # -> target/generate-rpm/steer-*.rpm
```

## Homebrew (tap)

1. Create a new GitHub repo named `homebrew-steer` (the `homebrew-` prefix is what makes `brew tap` find it).
2. Add `Formula/steer.rb` there, pointing at the release tarball URLs the workflow above produces, with each one's `sha256` (from the release's checksums, or `shasum -a 256` on the downloaded tarball).
3. Users install with `brew tap fclaude/steer && brew install steer`.

A single formula can cover both macOS binaries with an `on_arm`/`on_intel` block. Once there's a real release to point at, ask me to draft the formula.

## Fedora (COPR)

1. Create an account at [copr.fedorainfracloud.org](https://copr.fedorainfracloud.org) and a new project (e.g. `fclaude/steer`).
2. Point it at a `.spec` file (COPR can build from a source tarball + spec, or from the `.rpm` this workflow already produces).
3. Users install with `dnf copr enable fclaude/steer && dnf install steer`.

## Arch (AUR)

1. Create an AUR account and register an SSH key at [aur.archlinux.org](https://aur.archlinux.org).
2. Write (or generate with `cargo-aur`) a `PKGBUILD` — most Rust CLIs publish a `steer-bin` package that just downloads the release tarball, rather than building from source, since it installs instantly.
3. `git push` it to the AUR's git remote for the package.
4. Users install with `yay -S steer` or `paru -S steer` (or any other AUR helper).
