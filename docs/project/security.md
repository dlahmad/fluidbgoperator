---
title: Security Policy
---

# Security Policy

## Supported Versions

This project currently supports the latest `main` branch and tagged releases
from `v0.1.0` onward.

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
