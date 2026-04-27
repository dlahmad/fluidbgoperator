---
title: Release
---

# Release

Releases are tag driven.

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds stripped static musl release binaries, publishes
multi-architecture container image manifests to GHCR, and publishes the Helm
chart as both a workflow artifact and an OCI chart.

Images:

- `ghcr.io/<owner>/operator:<version>`
- `ghcr.io/<owner>/http:<version>`
- `ghcr.io/<owner>/rabbitmq:<version>`

Helm chart:

- `oci://ghcr.io/<owner>/charts/fluidbg-operator`
