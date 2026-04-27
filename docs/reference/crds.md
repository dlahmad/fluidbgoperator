---
title: CRDs
---

# CRDs

Generated CRDs live in `crds/` and are mirrored into the Helm chart under
`charts/fluidbg-operator/crds`.

Current API group and version:

```text
fluidbg.io/v1alpha1
```

Resources:

- `BlueGreenDeployment`
- `InceptionPlugin`
- `StateStore`

Regenerate CRDs after changing Rust CRD models:

```sh
cargo run --locked --bin gen-crds
cp crds/*.yaml charts/fluidbg-operator/crds/
```
