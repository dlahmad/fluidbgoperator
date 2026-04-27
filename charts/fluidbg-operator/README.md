# fluidbg-operator Helm Chart

Installs the FluidBG operator, CRDs, RBAC, service, and optional built-in
`InceptionPlugin` resources.

## Install

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

## Built-In Plugin Namespaces

`InceptionPlugin` is namespaced. Install built-in plugin CRs into every
namespace that contains `BlueGreenDeployment` resources referencing them:

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set builtinPlugins.namespaces='{fluidbg-system,checkout,payments}'
```

## Images

```yaml
operator:
  image:
    repository: ghcr.io/dlahmad/fbg-operator
    tag: 0.1.0

builtinPlugins:
  http:
    image:
      repository: ghcr.io/dlahmad/fbg-plugin-http
      tag: 0.1.0
  rabbitmq:
    image:
      repository: ghcr.io/dlahmad/fbg-plugin-rabbitmq
      tag: 0.1.0
```
