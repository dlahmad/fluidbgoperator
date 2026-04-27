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

The state store is operator-global runtime configuration, not a CRD selected
per `BlueGreenDeployment`.
`InceptionPlugin.spec.inceptor` describes the per-inception traffic component.
`InceptionPlugin.spec.manager` optionally references a privileged manager
Service in the operator namespace for resource create/delete operations.

Regenerate CRDs after changing Rust CRD models:

```sh
cargo run --locked --bin gen-crds
cp crds/*.yaml charts/fluidbg-operator/crds/
```
