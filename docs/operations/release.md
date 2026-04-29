---
title: Release
---

# Release

Releases are tag driven. The consolidated `CI/CD` workflow only publishes
release assets, GHCR release tags, multi-architecture image manifests, and the
OCI Helm chart when it runs from a pushed `v*.*.*` tag. Manual workflow runs and
normal branch pushes never create releases.

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The release path builds stripped static musl release binaries, publishes
multi-architecture container image manifests to GHCR, and publishes the Helm
chart as both a GitHub Release asset and an OCI chart.
Container images are not attached to the GitHub Release as image tarballs; GHCR
is the source for image distribution.

The amd64 and arm64 binaries are built on native GitHub-hosted Linux runners
with the matching musl Rust target. Do not run the Rust compiler inside an
emulated arm64 container for releases; that makes the build slow and brittle.
The release jobs publish per-architecture tags first and then assemble the
canonical multi-architecture manifests.

The same `build-binaries` artifacts feed the image build, e2e run, and release
asset upload. This avoids rebuilding the Rust executables independently in
multiple jobs. Cargo caches are shared by purpose:

- `cargo-host` for host checks, CRD generation, and e2e helper builds.
- `cargo-linux-amd64-musl` for static amd64 release binaries.
- `cargo-linux-arm64-musl` for static arm64 release binaries.

Image builds reuse the downloaded `fluidbg-dist-<arch>` executable artifacts.
BuildKit layer caches use the GitHub Actions cache backend, so cache entries are
not published as GHCR packages and do not become release artifacts.

GitHub Release assets contain executable archives and chart packages only:

- `fluidbg-<version>-linux-amd64.tar.gz`
- `fluidbg-<version>-linux-arm64.tar.gz`
- `fluidbg-operator-<version>.tgz`
- per-archive `.sha256` files
- `SHA256SUMS`

Images:

- `ghcr.io/<owner>/fbg-operator:<version>`
- `ghcr.io/<owner>/fbg-plugin-http:<version>`
- `ghcr.io/<owner>/fbg-plugin-rabbitmq:<version>`
- `ghcr.io/<owner>/fbg-plugin-azure-servicebus:<version>`

Example/test application images are intentionally not release artifacts. Build
them locally for kind demos or publish them to your own registry if needed.

Helm chart:

- `oci://ghcr.io/<owner>/charts/fluidbg-operator`
