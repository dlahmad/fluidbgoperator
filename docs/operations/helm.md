---
title: Helm Installation
---

# Helm Installation

The chart at `charts/fluidbg-operator` installs:

- `fluidbg.io/v1alpha1` CRDs.
- Operator Deployment, Service, ServiceAccount, ClusterRole, and ClusterRoleBinding.
- Optional built-in `InceptionPlugin` resources for HTTP and RabbitMQ.

## Basic Install

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

## Production Image Values

```yaml
operator:
  image:
    repository: ghcr.io/your-org/operator
    tag: 0.1.0

builtinPlugins:
  http:
    image:
      repository: ghcr.io/your-org/http
      tag: 0.1.0
  rabbitmq:
    image:
      repository: ghcr.io/your-org/rabbitmq
      tag: 0.1.0
```

## Plugin Namespaces

`InceptionPlugin` is namespaced. The operator resolves plugin references in the
same namespace as the `BlueGreenDeployment`. Install plugin CRs into every
namespace that should use the built-in plugins:

```yaml
builtinPlugins:
  install: true
  namespaces:
    - fluidbg-system
    - checkout
    - payments
```

## CRD Upgrades

Helm installs CRDs from `charts/fluidbg-operator/crds`. Helm does not remove
CRDs on uninstall and has conservative CRD upgrade behavior. For breaking CRD
changes, apply the generated CRDs explicitly before upgrading the chart:

```sh
kubectl apply -f crds/
helm upgrade fluidbg charts/fluidbg-operator -n fluidbg-system
```

