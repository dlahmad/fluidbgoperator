---
title: Contributing
---

# Contributing

## Development Checks

Run the local gate before opening a PR:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
helm lint ./charts/fluidbg-operator
```

If CRD model structs change, regenerate and mirror CRDs:

```sh
cargo run --locked --bin gen-crds
cp crds/blue_green_deployment.yaml charts/fluidbg-operator/crds/blue_green_deployment.yaml
cp crds/inception_plugin.yaml charts/fluidbg-operator/crds/inception_plugin.yaml
```

## E2E

The full kind-based suite is the behavioral gate:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```

This runs the Rust `fluidbg-e2e-tests` integration crate. Use
`cargo test -p fluidbg-e2e-tests --test e2e -- --ignored --test-threads=1 --nocapture`
when you want to call the harness directly.

## Code Standards

- Keep plugin wire types in `sdk/rust` and `sdk/spec` instead of duplicating them in plugins.
- Keep transport behavior inside plugins; the operator should orchestrate, not hardcode transport semantics.
- Add tests for reconciliation state transitions and plugin contracts when changing rollout behavior.
- Do not rely on application payload fields for FluidBG routing semantics.
