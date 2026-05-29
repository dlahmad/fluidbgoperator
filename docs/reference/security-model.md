---
title: Security Model
---

# Security Model

FluidBG separates cluster-level installation authority from per-rollout author
authority.

The default Helm installation requires Kubernetes v1.30 or newer because it
installs `ValidatingAdmissionPolicy` resources in
`admissionregistration.k8s.io/v1`.

The operator and plugin managers run with elevated permissions because they
must watch BGDs cluster-wide, create temporary rollout resources, and, for
managed transport plugins, create/delete broker infrastructure. A BGD author
must not get extra namespace power just because the operator has it. The Helm
chart therefore installs a `ValidatingAdmissionPolicy` that checks the actual
user or ServiceAccount creating/updating a `BlueGreenDeployment`.

## Admission Boundary

By default, Kubernetes admission rejects `BlueGreenDeployment` create/update
requests unless the requesting principal can perform the same namespace-scoped
Kubernetes actions the operator will perform for that BGD.

Required permissions:

| Scope | Resource | Required verbs | Why |
|---|---|---|---|
| BGD namespace | `apps/deployments` | `create`, `update`, `patch`, `delete` | Generated inceptor and verifier Deployments. |
| `spec.deployment.namespace`, or BGD namespace if omitted | `apps/deployments` | `create`, `update`, `patch`, `delete` | Candidate Deployment creation, update, promotion, rollback, and cleanup. |
| `spec.selector.namespace`, or BGD namespace if omitted | `apps/deployments` | `get`, `list`, `update`, `patch`, `delete` | Selecting, labeling, restoring, promoting, or deleting the current green Deployment. |
| BGD namespace | `services` | `create`, `update`, `patch`, `delete` | Generated inceptor and verifier Services. |
| BGD namespace | `configmaps` | `create`, `update`, `patch`, `delete` | Generated inceptor config and rollout snapshot ConfigMaps. |
| BGD namespace | `secrets` | `create`, `update`, `patch`, `delete` | Generated per-inception auth Secrets and verifier token Secret. |
| BGD namespace | `pods` | `list`, `delete` | Forced-delete/orphan cleanup of FluidBG-labeled leftover Pods. |

This policy intentionally does not inspect Pod templates for Secret references,
ConfigMap references, volume types, security context fields, or similar Pod
details. Those fields are native Kubernetes Deployment fields and should be
governed by normal Kubernetes RBAC and admission controls such as Pod Security
Admission, Kyverno, Gatekeeper, or organization-specific policies. FluidBG
should not be stricter or looser than Kubernetes for those fields.

`ValidatingAdmissionPolicy` is available as `admissionregistration.k8s.io/v1`
on Kubernetes v1.30 and newer. If a cluster cannot use it, set:

```yaml
admissionPolicy:
  enabled: false
```

Only disable it when an equivalent external admission policy enforces the same
privilege parity.

## Generated Resource Ownership

FluidBG generated resources are labeled with:

```text
fluidbg.io/blue-green-ref=<bgd-name>
fluidbg.io/blue-green-uid=<bgd-uid>
```

Before server-side applying generated Deployments, Services, ConfigMaps, or
Secrets, the operator checks any existing object with the same name. It refuses
to take over an object unless the existing labels match the same BGD reference
and UID. This prevents accidental or malicious name collision takeover.

Candidate application Deployments are different: they are user-declared rollout
targets. Their safety boundary is the admission policy and the user's normal
Deployment permissions in the target namespace.

## Secret Handling

The operator signing key lives in the operator namespace and is selected by the
operator installer. It is not copied into application namespaces.

Per-inception runtime credentials are stored in generated namespace-local
Secrets:

- `FLUIDBG_PLUGIN_AUTH_TOKEN` is read by inceptors from a generated Secret.
- Manager-returned inceptor credentials are stored in the same per-inception
  Secret and injected with `secretKeyRef`.
- Verifier token maps are stored in a generated verifier Secret and injected as
  `FLUIDBG_VERIFIER_AUTH_TOKENS_JSON`.

Inceptors receive tokens and scoped runtime credentials only. They do not
receive the signing key. Plugin managers may receive privileged infrastructure
credentials, but they run in the operator namespace and must authenticate
operator calls before creating/deleting external resources.

## Transport Credentials

RabbitMQ, NATS JetStream, and Azure Service Bus BGD config must not contain base
broker or cloud credentials. Those credentials belong to the plugin manager
installation. The manager receives the BGD context over an authenticated
lifecycle call and returns only per-inception inceptor environment values.
Temporary resource names and scoped credentials are derived from token claims
and active BGD context, not trusted from user-supplied temporary names.

The HTTP plugin has no external infrastructure manager. Its inceptor endpoints
still require the per-inception bearer token on lifecycle, writer, and verifier
callback paths.

## Cleanup Model

Normal delete uses the `fluidbg.io/cleanup` finalizer. The operator drains
plugins, removes generated inceptor/verifier resources, deletes store records,
and then removes the finalizer.

Forced-delete recovery is handled by orphan cleanup. The operator periodically
compares existing BGD CRs with store refs and FluidBG-labeled Kubernetes
resources. If a BGD no longer exists, it removes remaining labeled resources
and store records under a per-BGD Kubernetes Lease so multiple operator replicas
do not race each other.
