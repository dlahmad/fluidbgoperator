This folder gives you a verified manual apply sequence for the current controller behavior.

Apply order:

1. `./reset.sh`
2. `00-prereqs.yaml`
3. `01-bootstrap-from-empty-bgd.yaml`
4. `02-upgrade-bgd.yaml`
5. `03-trigger-upgrade-tests.yaml`

Short reset:

```bash
./reset.sh
```

Image prerequisite:

```bash
kind load docker-image fluidbg/fbg-operator:dev --name <your-kind-cluster>
kind load docker-image fluidbg/fbg-plugin-rabbitmq:dev --name <your-kind-cluster>
kind load docker-image fluidbg/green-app:dev --name <your-kind-cluster>
kind load docker-image fluidbg/blue-app:dev --name <your-kind-cluster>
kind load docker-image fluidbg/test-app:dev --name <your-kind-cluster>
```

Manual apply order:

1. `00-prereqs.yaml`
2. `01-bootstrap-from-empty-bgd.yaml`
3. `02-upgrade-bgd.yaml`
4. `03-trigger-upgrade-tests.yaml`

What works today:

- `01-bootstrap-from-empty-bgd.yaml` bootstraps the first green deployment when the selector matches nothing.
- `02-upgrade-bgd.yaml` updates the same `BlueGreenDeployment` object and starts a new rollout against the current green deployment.
- `03-trigger-upgrade-tests.yaml` only publishes five queue messages to RabbitMQ. It does not call the test container or the operator directly.
- The RabbitMQ observer plugin registers `testCase`s with the operator and notifies the test container through `observer.notifyPath`.
- The operator polls `tests[].dataVerification.verifyPath` on the test container and promotes when the configured data thresholds are met.
- If that deployment is not labeled `fluidbg.io/green=true`, the operator adopts it first and then continues.
- The operator chooses concrete deployment names itself and writes them to `status.generatedDeploymentName`.
- The test container and standalone plugin deployments are cleaned up after promotion.

Expected flow:

1. `00-prereqs.yaml` installs the operator, RabbitMQ, httpbin, the RabbitMQ plugin CR, and the in-memory state store.
2. `01-bootstrap-from-empty-bgd.yaml` creates the first generated deployment and marks it `fluidbg.io/green=true`.
3. `02-upgrade-bgd.yaml` creates a second generated deployment, a test container, and the standalone plugin deployments.
4. `03-trigger-upgrade-tests.yaml` publishes queue messages to `orders`.
5. The rollout reaches `Completed`, the previous green deployment is deleted, and the promoted deployment carries `fluidbg.io/green=true`.

Useful checks during the run:

```bash
kubectl get bluegreendeployment order-processor-bootstrap -n fluidbg-test -o jsonpath='{.status.phase}'; echo
kubectl get bluegreendeployment order-processor-bootstrap -n fluidbg-test -o jsonpath='{.status.generatedDeploymentName}'; echo
kubectl get deploy -n fluidbg-test --show-labels
kubectl get pods -n fluidbg-test
kubectl logs deployment/fluidbg-incoming-orders -n fluidbg-test
kubectl logs deployment/fluidbg-operator -n fluidbg-system
```

Final status example:

```bash
kubectl get bluegreendeployment order-processor-bootstrap -n fluidbg-test -o jsonpath='{.status.phase}'; echo
kubectl get bluegreendeployment order-processor-bootstrap -n fluidbg-test -o jsonpath='{.status.testCasesObserved}'; echo
kubectl get bluegreendeployment order-processor-bootstrap -n fluidbg-test -o jsonpath='{.status.testCasesPassed}'; echo
```
