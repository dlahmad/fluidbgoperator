---
title: Helm Installation
---

# Helm Installation

The chart at `charts/fluidbg-operator` installs:

- `fluidbg.io/v1alpha1` CRDs.
- Operator Deployment, Service, ServiceAccount, ClusterRole, and ClusterRoleBinding.
- Optional built-in `InceptionPlugin` resources for HTTP, RabbitMQ, and Azure Service Bus.

## Basic Install

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

## Production Image Values

The checked-in chart defaults point at this repository's GHCR packages. For a
fork, replace `dlahmad` with the publishing owner.

```yaml
operator:
  image:
    repository: ghcr.io/dlahmad/fbg-operator
    tag: 0.1.0
  auth:
    signingSecretNamespace: fluidbg-system
    signingSecretName: fluidbg-operator-auth
    signingSecretKey: signing-key

builtinPlugins:
  http:
    image:
      repository: ghcr.io/dlahmad/fbg-plugin-http
      tag: 0.1.0
  rabbitmq:
    image:
      repository: ghcr.io/dlahmad/fbg-plugin-rabbitmq
      tag: 0.1.0
  azureServiceBus:
    inceptorWorkloadIdentity:
      enabled: false
      serviceAccountName: ""
    manager:
      enabled: false
      workloadIdentity:
        enabled: false
        serviceAccountName: ""
    image:
      repository: ghcr.io/dlahmad/fbg-plugin-azure-servicebus
      tag: 0.1.0
```

## State Store

One operator instance uses one global state store backend for all watched
`BlueGreenDeployment` resources. The chart configures that backend on the
operator Deployment. The default is suitable for local development and e2e:

```yaml
stateStore:
  type: memory
```

For production, use Postgres:

```yaml
stateStore:
  type: postgres
  postgres:
    urlSecretName: fluidbg-postgres
    urlSecretKey: url
    tableName: fluidbg_cases
```

For local experiments, `stateStore.postgres.url` can be set directly instead of
using a Secret. Production installs should use `urlSecretName`.

## Plugin Managers

RabbitMQ and Azure Service Bus can use split plugin mode:

- The manager runs once in the operator namespace and owns privileged resource
  create/delete credentials.
- The inceptor is spawned per inception point in the application namespace and
  receives only the secured per-inception config and JWT.

The signing Secret is mounted/read only by the operator and enabled managers.
Inceptors do not receive the signing key; they receive only
`FLUIDBG_PLUGIN_AUTH_TOKEN` and require the same token on their own lifecycle
endpoints.

Enable a manager only after providing the required privileged credential source.
The built-in `InceptionPlugin` manager reference is rendered only when the
matching manager is enabled, so the chart does not create dangling manager
endpoints by default.

RabbitMQ manager example:

```yaml
builtinPlugins:
  rabbitmq:
    manager:
      enabled: true
      amqpUrlSecretName: rabbitmq-admin
      amqpUrlSecretKey: amqp-url
```

For Azure Service Bus workload identity, create a manager ServiceAccount in the
operator namespace and annotate it for Microsoft Entra Workload ID. Privileged
Azure/RabbitMQ infrastructure credentials should be mounted only into manager
deployments in the operator namespace, not into per-inception inceptor pods in
application namespaces. Then enable the manager pod label and ServiceAccount
reference:

```yaml
builtinPlugins:
  azureServiceBus:
    manager:
      enabled: true
      fullyQualifiedNamespace: my-namespace.servicebus.windows.net
      workloadIdentity:
        enabled: true
        serviceAccountName: fluidbg-azure-servicebus
        podAnnotations:
          azure.workload.identity/service-account-token-expiration: "3600"
```

## Plugin Namespaces

`InceptionPlugin` is namespaced. The operator resolves plugin references in the
same namespace as the `BlueGreenDeployment`. Install built-in plugin CRs into
every namespace that should use the built-in chart resources:

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
