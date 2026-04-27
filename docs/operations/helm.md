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

## State Store And HA

One operator instance uses one global state store backend for all watched
`BlueGreenDeployment` resources. The chart configures that backend on the
operator Deployment. The default is suitable for local development and e2e:

```yaml
stateStore:
  type: memory
```

`memory` is intentionally not HA-safe because each operator pod would hold a
different in-process store. The chart blocks `operator.replicaCount > 1` when
`stateStore.type=memory`.

For HA, use Postgres or Azure Cosmos DB.

Postgres with a password/connection string:

```yaml
operator:
  replicaCount: 2
stateStore:
  type: postgres
  postgres:
    authMode: password
    urlSecretName: fluidbg-postgres
    urlSecretKey: url
    tableName: fluidbg_cases
```

Postgres with AKS workload identity:

```yaml
operator:
  replicaCount: 2
serviceAccount:
  annotations:
    azure.workload.identity/client-id: <managed-identity-client-id>
stateStore:
  type: postgres
  postgres:
    authMode: workloadIdentity
    host: myserver.postgres.database.azure.com
    database: fluidbg
    user: fluidbg-app
    tableName: fluidbg_cases
```

The managed identity must be configured as an Azure Database for PostgreSQL
Microsoft Entra principal and granted the required database role permissions.
The operator requests an Entra token for Azure Database for PostgreSQL and uses
that token as the PostgreSQL password.

Azure Cosmos DB with a connection string:

```yaml
operator:
  replicaCount: 2
stateStore:
  type: cosmosdb
  cosmos:
    authMode: connectionString
    connectionStringSecretName: fluidbg-cosmos
    connectionStringSecretKey: connection-string
    database: fluidbg
    container: testcases
```

Azure Cosmos DB with workload identity:

```yaml
operator:
  replicaCount: 2
serviceAccount:
  annotations:
    azure.workload.identity/client-id: <managed-identity-client-id>
stateStore:
  type: cosmosdb
  cosmos:
    authMode: workloadIdentity
    endpoint: https://myaccount.documents.azure.com:443
    database: fluidbg
    container: testcases
```

The Cosmos DB container must already exist and use `/blue_green_ref` as the
partition key. For workload identity, grant the managed identity a Cosmos DB
data-plane RBAC role that can read, create, replace, query, and delete items in
that container. For local experiments, direct `stateStore.postgres.url`,
`stateStore.cosmos.connectionString`, or `stateStore.cosmos.accountKey` values
can be set, but production installs should reference Kubernetes Secrets.

Store cleanup is automatic. Pending cases are kept while a rollout CR still
exists and can need them. When a `BlueGreenDeployment` reaches a terminal state
or is deleted through the normal Kubernetes finalizer path, the operator removes
temporary resources and deletes all store records for that BGD.

The operator also runs orphan cleanup for forced-delete recovery. If a user
removes the finalizer or deletes the CR while the operator is down, the next
operator run compares store refs and `fluidbg.io/blue-green-ref` labeled
resources with existing BGD CRs. Refs whose CR no longer exists are cleaned from
Kubernetes and from the store. Unpromoted candidate deployments are labeled for
this cleanup path; the label is removed when a candidate becomes the active
green deployment. Cleanup operations are idempotent so multiple operator
replicas can run them safely.

The orphan cleanup interval defaults to 60 seconds and can be adjusted with:

```yaml
operator:
  orphanCleanup:
    intervalSeconds: 60
```

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
