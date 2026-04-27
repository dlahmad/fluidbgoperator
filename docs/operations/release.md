---
title: Release
---

# Release

Releases are tag driven and can also be started manually from the `Release`
workflow with a `version` input.

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds stripped static musl release binaries, publishes
multi-architecture container image manifests to GHCR, and publishes the Helm
chart as both a workflow artifact and an OCI chart.

Images:

- `ghcr.io/<owner>/fbg-operator:<version>`
- `ghcr.io/<owner>/fbg-plugin-http:<version>`
- `ghcr.io/<owner>/fbg-plugin-rabbitmq:<version>`

Helm chart:

- `oci://ghcr.io/<owner>/charts/fluidbg-operator`
