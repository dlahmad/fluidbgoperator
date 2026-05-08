---
title: Security Policy
---

# Security Policy

## Supported Versions

This project currently supports the latest `main` branch and the latest tagged
release.

## Reporting Vulnerabilities

Do not open public issues for suspected vulnerabilities. Report privately to the
project maintainers.

Include:

- affected version or commit
- reproduction steps
- expected impact
- relevant logs or manifests with secrets removed

## Runtime Defaults

The Helm chart runs the operator as non-root, drops Linux capabilities, uses a
read-only root filesystem, and applies the Kubernetes `RuntimeDefault` seccomp
profile.

## Runtime Authorization Model

The operational security model is documented in
[Security Model](../reference/security-model.md). In short, the Helm chart
installs a `ValidatingAdmissionPolicy` so a user can create or update a
`BlueGreenDeployment` only when that same user has the Kubernetes permissions
needed for the namespace-scoped Deployments, Services, ConfigMaps, Secrets, and
FluidBG-labeled Pod cleanup that the operator performs for that rollout.
