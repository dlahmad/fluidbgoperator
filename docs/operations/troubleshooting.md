---
title: Troubleshooting
---

# Troubleshooting

Every reconciler failure is surfaced on the `BlueGreenDeployment` status. The
operator still writes logs, but the status is the primary user interface for
diagnosis and GitOps health.

## First Checks

```sh
kubectl get bgd <name> -n <namespace> -o wide
kubectl get bgd <name> -n <namespace> -o yaml
kubectl describe bgd <name> -n <namespace>
```

Inspect these fields:

| Field | Meaning |
|---|---|
| `status.phase` | Rollout state. `Invalid` means the current spec generation cannot run. |
| `status.conditions[]` | User-facing diagnostics. The important fields are `type`, `status`, `reason`, `message`, and `observedGeneration`. |
| `status.conditions[?type=ReconcileFailed]` | Latest reconcile failure. `reason` is stable enough for automation; `message` explains the concrete failure. |
| `status.conditions[?type=Degraded]` | Overall failure/degraded signal for GitOps and dashboards. |
| `status.inceptionPointDrains[]` | Per-inception drain progress and drain timeout diagnostics. |
| `status.lastFailureMessage` | Latest verifier/test-case failure message, when the rollout reached observation. |

## Error Reasons

| Reason | Phase behavior | Meaning | Typical action |
|---|---|---|---|
| `InvalidSpec` | Sets `phase: Invalid` for the current generation. | The BGD spec or referenced plugin contract is invalid. Examples: unsupported role, mutually exclusive role combination, invalid plugin config schema, missing promotion fields, progressive strategy without a compatible splitter. | Fix the YAML and apply a new generation. |
| `KubernetesApiError` | Requeues and keeps the current rollout phase. | The operator could not read, create, patch, or delete a Kubernetes object. | Check RBAC, namespaces, CRDs, API-server errors, and referenced resources. |
| `ResourceError` | Requeues and keeps the current rollout phase. | The operator could not render or reconcile a required Kubernetes resource from the BGD/plugin inputs. | Check deployment specs, service specs, custom resources, generated names, and object templates. |
| `PluginManagerError` | Requeues and keeps the current rollout phase. | A privileged plugin manager call failed or returned an invalid response. | Check manager pod readiness, manager service, manager auth token, and manager credentials. |
| `PluginInceptorError` | Requeues and keeps the current rollout phase. | An inceptor lifecycle call failed or returned an invalid response. | Check inceptor pod logs, service endpoints, auth token handling, and `/prepare`, `/activate`, `/drain`, `/cleanup` handlers. |
| `PluginDrainStatusError` | Requeues and keeps the current rollout phase. | The operator could not read drain status from an inceptor. | Check the inceptor `/drain-status` endpoint and transport access. |
| `PluginTrafficShiftError` | Requeues and keeps the current rollout phase. | A progressive traffic shift call failed. | Check splitter inceptor readiness and `/traffic` handling. |
| `LeaseError` | Requeues and keeps the current rollout phase. | This operator lost the per-BGD lease during reconcile. | Usually transient in HA mode. Check operator restarts and lease settings if repeated. |
| `AuthError` | Requeues and keeps the current rollout phase. | The operator could not sign or validate plugin JWT auth. | Check the configured signing secret, key name, and Helm auth settings. |
| `StateStoreError` | Requeues and keeps the current rollout phase. | The state backend failed while reading/writing test cases or rollout state. | Check Postgres/Cosmos/memory-store configuration, credentials, connectivity, and HA compatibility. |
| `ControllerError` | Requeues and keeps the current rollout phase. | An internal controller invariant failed while processing generated rollout state. | Treat as an operator bug unless the message points to malformed custom resource content. Include the condition message and operator logs in the issue. |

## Role Combination Errors

Role compatibility is declared by the selected `InceptionPlugin`, not hardcoded
by the operator. The built-in RabbitMQ and Azure Service Bus plugins declare
`duplicator`, `splitter`, `combiner`, and `consumer` as mutually exclusive
movement roles. `observer` and `writer` are additive.

Invalid:

```yaml
roles: [splitter, combiner, observer]
```

Expected status:

```yaml
status:
  phase: Invalid
  conditions:
    - type: ReconcileFailed
      status: "True"
      reason: InvalidSpec
      message: "invalid spec: unsupported role combination: roles splitter, combiner are mutually exclusive ..."
```

Valid:

```yaml
roles: [splitter, observer, writer]
```

## GitOps Behavior

`Completed` reports `Ready=True`, `Progressing=False`, and `Degraded=False`.
`RolledBack` and `Invalid` report `Ready=False`, `Progressing=False`, and
`Degraded=True`.

Transient reconcile failures add `ReconcileFailed=True` and `Degraded=True`
while the operator keeps retrying. A later successful phase update replaces the
condition set and clears the stale failure.
