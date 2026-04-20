# FluidBG Operator — Implementation Plan

This document is a concrete, phased plan for implementing the FluidBG operator described in `ARCHITECTURE.md`. It is written for an autonomous implementer (human or LLM) who will build the system end-to-end, with **integration tests as a first-class deliverable at every phase**.

## Target Environment & Assumptions

- **Language**: Rust (edition 2024), operator built with `kube-rs`.
- **Kubernetes**: local cluster via **kind** (Kubernetes in Docker). No cloud dependency.
- **Messaging backend for tests**: **RabbitMQ** (deployed in the cluster via a Helm chart or raw manifests).
- **State-store backend for tests**: **PostgreSQL** (deployed in the cluster via a Helm chart or raw manifests).
- **Host OS**: macOS or Linux with Docker Desktop (or native Docker on Linux).
- Both RabbitMQ and Postgres are installed by the implementer as part of the environment; they are not managed by FluidBG.

The plan assumes the implementer has these tools on `PATH`:

| Tool | Purpose |
|---|---|
| `docker` | Image building, runtime for kind |
| `kind` | Local Kubernetes cluster |
| `kubectl` | Cluster interaction |
| `helm` | Installing RabbitMQ + Postgres |
| `cargo` | Rust toolchain |
| `just` or `make` (optional) | Task runner |

---

## Guiding Principles

1. **Integration-first.** Every phase ships integration tests that run against real infrastructure (kind + RabbitMQ + Postgres). Unit tests are welcome, but a phase is not "done" until its integration tests pass.
2. **CRD-driven.** The operator never hardcodes plugin types. The generic reconciler works from `InceptionPlugin` CRDs. A phase that adds a plugin adds a YAML manifest + a container image.
3. **Small vertical slices.** Each phase produces an end-to-end path, even if narrow, rather than horizontal layers that don't connect.
4. **Reproducible test cluster.** `just test-env-up` / `just test-env-down` scripts bring the cluster up and down identically across machines.
5. **No flaky tests.** Tests that depend on timing use deterministic waits (polling with timeouts), not fixed sleeps.

---

## Repository Layout (target)

```
fluidbgoperator/
├── Cargo.toml                      # workspace root
├── Cargo.lock
├── operator/                       # the operator binary crate
│   ├── Cargo.toml
│   └── src/                        # as in ARCHITECTURE.md → Project Structure
├── plugins/
│   ├── http_proxy/
│   │   ├── Cargo.toml
│   │   ├── Dockerfile
│   │   └── src/
│   ├── http_writer/
│   ├── rabbitmq_duplicator/
│   └── rabbitmq_writer/
├── crds/                           # YAML CRD manifests (schemars-generated)
│   ├── inception_plugin.yaml
│   ├── state_store.yaml
│   └── blue_green_deployment.yaml
├── builtin-plugins/                # YAML InceptionPlugin CRDs for built-in plugins
│   ├── http_proxy.yaml
│   ├── http_writer.yaml
│   ├── rabbitmq_duplicator.yaml
│   └── rabbitmq_writer.yaml
├── deploy/                         # Operator Deployment, RBAC, ServiceAccount
│   ├── operator.yaml
│   └── rbac.yaml
├── tests/
│   ├── integration/                # cross-phase integration tests
│   └── e2e/                        # full end-to-end tests
├── testenv/                        # kind config, RabbitMQ/Postgres manifests
│   ├── kind-config.yaml
│   ├── postgres.yaml
│   └── rabbitmq.yaml
├── justfile                        # or Makefile
├── ARCHITECTURE.md
└── IMPLEMENTATION_PLAN.md
```

---

## Environment Setup (Phase 0.1)

### 1. Create the kind cluster

`testenv/kind-config.yaml`:

```yaml
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: fluidbg-dev
nodes:
  - role: control-plane
    extraPortMappings:
      - containerPort: 30080            # reserved for test-container NodePort
        hostPort: 30080
      - containerPort: 30090            # reserved for operator API NodePort
        hostPort: 30090
```

```
kind create cluster --config testenv/kind-config.yaml
kubectl create namespace fluidbg-system
kubectl create namespace fluidbg-test
```

### 2. Install RabbitMQ (via Bitnami Helm chart)

`testenv/rabbitmq.yaml` (values file):

```yaml
auth:
  username: fluidbg
  password: fluidbg
  erlangCookie: secretcookie
persistence:
  enabled: false
service:
  type: ClusterIP
```

```
helm repo add bitnami https://charts.bitnami.com/bitnami
helm repo update
helm upgrade --install rabbitmq bitnami/rabbitmq \
  -n fluidbg-system -f testenv/rabbitmq.yaml \
  --wait
```

Connection URL from inside the cluster: `amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/`.

### 3. Install PostgreSQL (via Bitnami Helm chart)

`testenv/postgres.yaml` (values file):

```yaml
auth:
  username: fluidbg
  password: fluidbg
  database: fluidbg
primary:
  persistence:
    enabled: false
```

```
helm upgrade --install postgres bitnami/postgresql \
  -n fluidbg-system -f testenv/postgres.yaml \
  --wait
```

Connection URL from inside the cluster: `postgres://fluidbg:fluidbg@postgres-postgresql.fluidbg-system:5432/fluidbg`.

### 4. Provide a `justfile` that wraps the above

```makefile
test-env-up:
    kind create cluster --config testenv/kind-config.yaml
    kubectl create namespace fluidbg-system
    kubectl create namespace fluidbg-test
    helm repo add bitnami https://charts.bitnami.com/bitnami
    helm upgrade --install rabbitmq bitnami/rabbitmq \
      -n fluidbg-system -f testenv/rabbitmq.yaml --wait
    helm upgrade --install postgres bitnami/postgresql \
      -n fluidbg-system -f testenv/postgres.yaml --wait

test-env-down:
    kind delete cluster --name fluidbg-dev

kind-load IMAGE:
    kind load docker-image {{IMAGE}} --name fluidbg-dev
```

### Integration Test for Phase 0.1

`tests/integration/env_smoke_test.rs` (run against the cluster):

- Assert a pod can be created in `fluidbg-test` that connects to RabbitMQ (publishes and consumes one message).
- Assert a pod can connect to Postgres (runs `SELECT 1`).

A single test harness script `cargo test -p operator --test env_smoke_test --features integration-tests` confirms the env is wired.

---

## Phase 0.2 — Rust Workspace Skeleton

### Deliverables

- Cargo workspace with `operator/`, `plugins/*` crates.
- Operator crate's `main.rs` prints "fluidbg operator vX.Y starting".
- CI pipeline stub (GitHub Actions file) that runs `cargo fmt --check && cargo clippy -- -D warnings && cargo test --workspace` in a kind-backed job.
- `Dockerfile` for the operator image (multi-stage, `FROM gcr.io/distroless/cc-debian12` runtime).

### Integration Test

- Build the operator image, load into kind, deploy as a `Deployment` with minimal RBAC, assert pod reaches `Ready`.

---

## Phase 1 — CRDs + Static Validation

### Deliverables

- `crd::inception_plugin::InceptionPlugin` with `schemars` derive, matching `ARCHITECTURE.md` exactly.
- `crd::state_store::StateStore` (types: `memory`, `redis`, `postgres`).
- `crd::blue_green::BlueGreenDeployment` (including `inceptionPoints`, `tests`, `promotion`, `status`).
- `operator/src/crd/bin/gen_crds.rs` binary that writes YAML manifests to `crds/` via `schemars`.
- YAML manifests committed in `crds/`.
- `deploy/rbac.yaml` granting the operator permissions on these CRDs + Deployments/Pods/ConfigMaps/Services.

### Key Validation Rules (enforced at CRD parse time in the operator)

1. **Mode × Direction** (see ARCHITECTURE table): reject invalid combinations.
2. **Field namespaces**: every filter/selector `field` must start with a namespace declared in the referenced plugin's `fieldNamespaces`.
3. **User config vs `configSchema`**: the plugin's `configSchema` (JSON Schema) must validate the `config` block.
4. **`testId` selector**: required for `trigger`, `passthrough-duplicate`, `reroute-mock`; optional for `write`.

### Integration Tests

`tests/integration/crd_validation_test.rs`:

- Apply the CRD manifests to the cluster.
- Apply a valid `InceptionPlugin` + valid `BlueGreenDeployment` — expect success.
- Apply each of these malformed resources — expect `kubectl apply` to fail or the operator to emit an `Invalid` status condition:
  - Inception point with `mode: reroute-mock` + `directions: [ingress]`.
  - Filter field `http.body` used with `rabbitmq-duplicator` (only supports `queue.*`).
  - `config` missing a `required` field declared in the plugin's `configSchema`.
  - Unknown plugin (`pluginRef.name: nonexistent`).

---

## Phase 2 — State Store (memory + postgres)

### Deliverables

- `state_store::StateStore` trait (see ARCHITECTURE.md).
- `state_store::memory::MemoryStore` — `tokio::sync::RwLock<HashMap>`-backed.
- `state_store::postgres::PostgresStore` using `sqlx` with a migrations directory.
- Schema:
  ```sql
  CREATE TABLE fluidbg_cases (
      test_id TEXT NOT NULL,
      blue_green_ref TEXT NOT NULL,
      triggered_at TIMESTAMPTZ NOT NULL,
      trigger_inception_point TEXT NOT NULL,
      timeout_seconds INTEGER NOT NULL,
      status TEXT NOT NULL,              -- Triggered|Observing|Passed|Failed|TimedOut
      verdict BOOLEAN,
      expires_at TIMESTAMPTZ NOT NULL,   -- for TTL scans
      PRIMARY KEY (blue_green_ref, test_id)
  );
  CREATE INDEX idx_fluidbg_cases_expires ON fluidbg_cases (expires_at)
      WHERE status IN ('Triggered', 'Observing');
  ```
- Factory function `StateStore::from_crd(&StateStore) -> Arc<dyn StateStore>` that reads the `StateStore` CRD and constructs the right backend.

### Unit Tests

- Property-based round-trip of the `StateStore` trait (`memory`), covering:
  - `register` → `get` returns the same record.
  - `set_verdict` updates status to `Passed`/`Failed`.
  - `mark_timed_out` updates status to `TimedOut`.
  - `list_pending` returns only `Triggered`/`Observing`.
  - `counts` matches the sum of verdicts per `blue_green_ref`.

### Integration Tests

`tests/integration/state_store_postgres_test.rs`:

- Run the exact same property-based suite against `PostgresStore` connected to the in-cluster Postgres (port-forwarded or run inside a test pod).
- `cleanup_expired` deletes rows where `expires_at < now()` and status is `Triggered`/`Observing` (not finalized ones).
- Concurrency: N tasks call `register` + `set_verdict` concurrently; final counts are correct.

---

## Phase 3 — Inception Tracking + Operator HTTP API

### Deliverables

- `http_api.rs`: `axum` server exposing:
  - `POST /cases` — plugins register a test run. Body: `{testId, blueGreenRef, inceptionPoint, triggeredAt, timeoutSeconds}`.
  - `GET /health`.
- `inception.rs`:
  - Background task that periodically polls every pending case by calling `GET {resultPath}` on the test container.
  - Background task that scans for timed-out cases and marks them `TimedOut`.
  - Uses the configured `StateStore`.
- Wire the HTTP API into `main.rs` on port 8090.

### Unit Tests

- Timeout logic: given a case registered at `t0` with `timeout=10s`, after 11 simulated seconds, `list_pending` returns it, and the poller would mark it timed out.
- Polling backoff: `reqwest` mock returns `{passed: null}` twice, then `{passed: true}` — operator records `Passed`.

### Integration Tests

`tests/integration/inception_flow_test.rs`:

- Deploy the operator + a mock plugin pod that `POST`s a case to the operator, and a mock test container pod that serves `GET /result/{id}` returning `{passed: true}` after a 2s delay.
- Assert: the operator eventually shows `counts.passed = 1` via the postgres state store.

Second test case:

- Mock plugin registers case with `timeoutSeconds: 2`; test container never returns a verdict; after 3s the operator marks it `TimedOut`. Verified via direct Postgres query.

---

## Phase 4 — Generic Plugin Reconciler

### Deliverables

- `plugins::reconciler`:
  - Given an `InceptionPlugin` CRD and one inception point from a `BlueGreenDeployment`, produces:
    - A `ConfigMap` containing the user's `config` (or the result of `configTemplate` rendering) + the plugin-wide metadata (mode, directions, testContainerUrl, operatorUrl).
    - Either a sidecar `Container` (for `sidecar-blue` / `sidecar-test`) or a `Deployment` + `Service` (for `standalone`).
    - The env-var injections into the blue container and/or test container.
- `plugins::schema`: validates user config against plugin `configSchema` using the `jsonschema` crate.
- `plugins::filter` + `plugins::selector` + `plugins::fields`: the shared engines (even though they execute in plugins, the operator validates the rules upfront).
- `plugins::template`: Tera (or `handlebars`) rendering of `configTemplate`.

### Key Design Test (critical)

Write a test that proves **adding a plugin requires no operator code changes**:

- Define a fictional `InceptionPlugin` YAML manifest with a fake image (`example.com/fake-plugin:1`).
- Give a `BlueGreenDeployment` that uses it.
- Run the reconciler in a unit test (no real cluster) and assert the generated `Deployment`/`Service`/`ConfigMap` have the expected shape derived entirely from the YAML (image, ports, mounts, env).

### Integration Tests

`tests/integration/reconciler_test.rs`:

- Apply the four built-in `InceptionPlugin` CRDs + a `BlueGreenDeployment` that references `http-proxy` and `rabbitmq-duplicator`.
- Wait for the operator to reconcile.
- Assert:
  - A ConfigMap `fluidbg-config-<inceptionPoint>` exists per inception point.
  - The blue pod spec has the http-proxy sidecar container.
  - The rabbitmq-duplicator has its own `Deployment` + `Service`.
  - The blue container has the declared env vars resolved to correct URLs.

---

## Phase 5 — RabbitMQ Duplicator & Writer Plugins

### Deliverables

- `plugins/rabbitmq_duplicator/`: Rust binary using `lapin` + `axum` healthz.
  - Reads `/etc/fluidbg/config.yaml`.
  - Consumes from `sourceQueue`; for matched messages: extracts `testId`; `POST`s to operator (`trigger` mode) or to test container (`passthrough-duplicate`); republishes to `shadowQueue`.
- `plugins/rabbitmq_writer/`: Rust binary with `axum` exposing `POST /write` on port 9090.
  - Body: `{testId, payload, properties}` — publishes to `targetQueue` via `lapin`.
- `builtin-plugins/rabbitmq_duplicator.yaml` and `rabbitmq_writer.yaml` `InceptionPlugin` CRDs.
- `Dockerfile` for each plugin (multi-stage, distroless).

### Unit Tests (per plugin crate)

- Filter-matching: parse a sample config, feed sample messages, assert match/no-match.
- TestId extraction from `queue.body` jsonPath and from `queue.property.*`.

### Integration Tests

`tests/integration/rabbitmq_duplicator_test.rs`:

1. Spin up a tiny helper `Deployment` that acts as a mock test container (records all POSTs).
2. Deploy the rabbitmq-duplicator as a standalone `Deployment` with a handcrafted ConfigMap.
3. Publish 3 matching messages + 2 non-matching to `orders` queue on in-cluster RabbitMQ.
4. Assert:
   - `orders-blue` queue received exactly the 3 matching messages.
   - The operator's `POST /cases` was called 3 times (verified via state-store contents — use `memory` for speed in this test).
   - The mock test container received 3 `POST /trigger` requests, each with the right `testId`.

`tests/integration/rabbitmq_writer_test.rs`:

1. Deploy the rabbitmq-writer.
2. Send `POST /write` with a payload targeting a test queue.
3. Consume from that queue and assert the payload arrived intact.

---

## Phase 6 — HTTP Proxy & HTTP Writer Plugins

### Deliverables

- `plugins/http_proxy/`: Rust sidecar using `hyper` + `tower`. Listens on `proxyPort`. For each request, evaluates filters/selectors; routes per mode/direction. Supports bidirectional (`directions: [ingress, egress]`) by listening on two ports if needed, or by distinguishing requests via Host header / path prefix — **simplest**: two separate ports, `ingressPort` + `egressPort`, both declared in config.
- `plugins/http_writer/`: standalone `axum` service exposing `POST /write`.
- `builtin-plugins/http_proxy.yaml` and `http_writer.yaml`.

### Unit Tests

- Filter matching on `http.method`, `http.path`, `http.header.*`, `http.query.*`, `http.body` + jsonPath.
- Path-segment extraction: `/orders/123/process` with `pathSegment: 2` → `"123"`.
- Payload modes: `request` / `response` / `both` produce the right POST body shape.

### Integration Tests

`tests/integration/http_proxy_test.rs`:

- Deploy a blue pod containing the http-proxy sidecar + a tiny echo server as "blue".
- Deploy a mock upstream service (returns `{"status": 200}`).
- Deploy a mock test container (records POSTs).
- Send an ingress request matching trigger rules → assert `/trigger` hit + request reached blue.
- Blue makes an egress call to the upstream (through the sidecar in passthrough-duplicate mode) → assert upstream received it + test container received the observation.
- Switch the same proxy to `reroute-mock` mode in a different test; blue's egress hits the test container and receives its response; the real upstream is **not** called (verified by the mock's hit counter staying at zero).

---

## Phase 7 — Evaluator + Promotion (Hard-Switch first)

### Deliverables

- `evaluator.rs`: reads counts from the state store; compares to `successCriteria.successRate` and `minCases`.
- `strategy::hard_switch`: given a green verdict, patch service selectors / update `greenRef` to blue, scale down blue-as-blue, tear down test container + plugin deployments.
- `strategy::progressive`: step-by-step traffic shifting — initially only implement the happy path with hard-coded percentage control via the duplicator's `trafficPercent` config field.
  - Duplicator must accept a `trafficPercent` field controlling what fraction of matched messages go to blue (the rest bypass, effectively only green handles them).
  - HTTP proxy must accept a `trafficPercent` field for splitting ingress requests to blue vs green.
- Operator status updates reflecting current phase and counts.

### Integration Tests

`tests/integration/promotion_hard_switch_test.rs`:

1. Apply a minimal `BlueGreenDeployment` using RabbitMQ duplicator + mock test container that always returns `passed: true`.
2. Publish 120 matching messages.
3. Assert:
   - All 120 become cases with verdict `Passed`.
   - `currentSuccessRate == 1.0`.
   - The operator transitions `phase: Observing → Promoting → Completed`.
   - Blue's deployment has been swapped to green.

`tests/integration/promotion_rollback_test.rs`:

1. Same setup, but the mock test container returns `passed: false` for 50% of cases.
2. Publish 120 messages; `successRate` will be ~0.5.
3. Assert: phase transitions to `RolledBack`; blue resources (deployment, sidecars, duplicator deployment, test container, ConfigMaps) are deleted; green is untouched (verify its pod UID is unchanged).

`tests/integration/promotion_progressive_test.rs`:

1. Use a progressive strategy with two steps: `{5%, 10 cases, 0.99}`, `{100%, 20 cases, 0.98}`.
2. Publish 200 messages; mock test container returns `true` for all.
3. Assert the operator advances from step 0 → step 1 → completed, and during step 0 only ~5% of matched messages reached blue (verify via message counter on blue's consumer).

---

## Phase 8 — End-to-End Example + Reference Test Container

### Deliverables

- `examples/order-processor/`: a sample "blue" service (tiny HTTP + RabbitMQ consumer).
- `examples/order-validation/`: a sample test container that implements the `trigger` / `observe` / `result` contract.
- `examples/bluegreen.yaml`: a `BlueGreenDeployment` that wires it all up with rabbitmq + http-proxy inception points.
- `README.md` walkthrough: "run `just demo` to see the full flow".

### End-to-End Test

`tests/e2e/order_flow_e2e_test.rs`:

1. Bring up the full env (`just test-env-up`).
2. `kubectl apply -f examples/`.
3. Publish 100 orders to RabbitMQ using the test harness.
4. Wait up to 5 minutes.
5. Assert the `BlueGreenDeployment` status reaches `Completed` with `successRate >= 0.98`.

---

## Cross-Cutting Testing Strategy

### Test Tiers

| Tier | Where | Infra Required | Run Frequency |
|---|---|---|---|
| Unit | alongside each module | none | every `cargo test` |
| Integration | `tests/integration/*` | kind + RabbitMQ + Postgres | PR + nightly |
| E2E | `tests/e2e/*` | kind + all the above + example images | nightly + release |

### Test Harness (`tests/common/`)

- `Cluster::current()` — returns a handle to the kind cluster (assumes `KUBECONFIG` is set).
- `wait_until<F, T>(timeout: Duration, poll: F)` — deterministic condition polling with clear failure messages.
- `install_crds()` / `uninstall_crds()` — idempotent helpers.
- `apply_yaml(&str)` / `delete_yaml(&str)`.
- `publish_to_rabbitmq(queue: &str, body: &[u8])`.
- `query_postgres(sql: &str) -> Vec<Row>`.

### Test Isolation

- Each test runs in its own Kubernetes namespace named after the test (e.g., `it-rabbitmq-duplicator-<random>`), created in setup and torn down in teardown.
- No test depends on state from another.
- All tests that use the in-cluster Postgres create their own table prefix to avoid clashes when run in parallel (CI runs with `--test-threads=1` for cluster tests, but still).

### CI

GitHub Actions workflow:

```
- name: Set up kind
  uses: helm/kind-action@v1.10.0
  with:
    cluster_name: fluidbg-dev
    config: testenv/kind-config.yaml
- name: Install RabbitMQ + Postgres
  run: just test-env-infra
- name: Build images
  run: docker build -t fluidbg/operator:ci . && ... ; kind load docker-image ...
- name: Run unit tests
  run: cargo test --workspace --lib
- name: Run integration tests
  run: cargo test --workspace --test '*' --features integration-tests
- name: Run E2E tests (nightly only)
  if: github.event_name == 'schedule'
  run: cargo test --workspace --test e2e_* --features e2e-tests
```

### Observability During Tests

Every test captures:

- `kubectl get events -A` snapshot on failure.
- Operator pod logs on failure.
- Plugin pod logs on failure.
- Postgres table contents on failure (for `fluidbg_cases`).

Implement this via a `on_failure_dump(namespace: &str)` helper in `tests/common/`.

---

## Acceptance Criteria

The project is done when:

1. All phases' integration tests pass on CI against kind + RabbitMQ + Postgres.
2. The E2E example (`examples/order-processor`) completes a full blue-green cycle end-to-end in under 5 minutes.
3. Adding a new plugin requires only: a new container image + a new `InceptionPlugin` CRD manifest. A demonstration of this (e.g., a trivial `null-plugin` that does nothing but satisfies the runtime contract) is included as a test.
4. `cargo fmt --check && cargo clippy -- -D warnings` is clean.
5. `ARCHITECTURE.md` is consistent with the shipped behavior; no drift.

---

## Recommended Order of Work

| Week | Focus |
|---|---|
| 1 | Phase 0 (env + skeleton) + Phase 1 (CRDs) |
| 2 | Phase 2 (state store) + Phase 3 (inception + HTTP API) |
| 3 | Phase 4 (plugin reconciler) |
| 4 | Phase 5 (RabbitMQ plugins) + first full RabbitMQ integration flow |
| 5 | Phase 6 (HTTP plugins) |
| 6 | Phase 7 (evaluator + hard switch, then progressive) |
| 7 | Phase 8 (E2E example, documentation, polish) |

---

## Risks & Known Unknowns

- **Traffic splitting in queue-based progressive strategy**: RabbitMQ has no native percentage-routing primitive. The duplicator implements it by probabilistic decisions in code. Must be seeded deterministically in tests.
- **Proxy transparency**: the HTTP proxy sidecar requires blue to use the injected env var. If blue hardcodes upstream URLs, it defeats the proxy. This is a user responsibility — document it clearly.
- **Reconciliation ordering**: plugin deployments must be Ready *before* blue is scaled up, otherwise blue's first requests hit nothing (for sidecar-blue this is automatic; for standalone it requires readiness gates or deliberate ordering in the reconciler).
- **Test container restart**: a crash loses observations. The operator detects this only via cases that time out. Document as a known limitation of the current test-container contract; a future version could add an "init" handshake.

---

## Glossary of Test IDs (for consistency across tests)

| Concept | Standard Name in Tests |
|---|---|
| Sample blue-green resource | `order-processor` |
| Sample trigger inception point | `incoming-orders` |
| Sample test container | `order-validation` |
| Sample testId prefix | `ORD-{n}` |
| Test namespace prefix | `it-<testname>` |
