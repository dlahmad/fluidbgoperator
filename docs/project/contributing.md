---
title: Contributing
---

# Contributing

## Development Checks

Run the local gate before opening a PR:

```sh
just check
bash -n e2e/run-test.sh
python -m py_compile e2e/blue-app/app.py e2e/green-app/app.py e2e/test-app/app.py
helm lint ./charts/fluidbg-operator
```

If CRD model structs change, regenerate and mirror CRDs:

```sh
just crds
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
