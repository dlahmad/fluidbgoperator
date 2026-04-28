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
`BlueGreenDeployment.spec.test` defines one verifier using native Kubernetes
`deployment` and `service` specs. Put env vars, readiness probes,
resources, security contexts, commands, and ports in those Kubernetes specs.
`BlueGreenDeployment.spec.updatePolicy.activeRollout` controls what happens
when a BGD is edited during a non-terminal rollout:

- `defer` is the default. The active rollout keeps using its internal spec
  snapshot and the new generation starts after `Completed` or `RolledBack`.
- `force-replace` interrupts the active test plan and immediately starts the
  same rollback-style drain used by a failed rollout. Plugins restore traffic to
  the current green path, drain temporary resources, and run cleanup before the
  new generation starts. This does not require any observed test cases; it is
  based on the active rollout resources. The interrupted rollout is marked
  `RolledBack`. It preserves drain guarantees but still carries the operational
  risk of interrupting candidate-side work.

Regenerate CRDs after changing Rust CRD models:

```sh
cargo run --locked --bin gen-crds
cp crds/blue_green_deployment.yaml charts/fluidbg-operator/crds/blue_green_deployment.yaml
cp crds/inception_plugin.yaml charts/fluidbg-operator/crds/inception_plugin.yaml
```
