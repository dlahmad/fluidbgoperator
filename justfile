fmt:
    cargo fmt --all --check

lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo test --workspace --locked

check: fmt lint test

crds:
    cargo run --locked --bin gen-crds
    cp crds/blue_green_deployment.yaml charts/fluidbg-operator/crds/blue_green_deployment.yaml
    cp crds/inception_plugin.yaml charts/fluidbg-operator/crds/inception_plugin.yaml
    cp crds/state_store.yaml charts/fluidbg-operator/crds/state_store.yaml

build-binaries:
    ./scripts/build-linux-binaries.sh

build-images tag="dev":
    ./scripts/build-images.sh --tag {{tag}}

helm-lint:
    helm lint charts/fluidbg-operator
    helm template fluidbg charts/fluidbg-operator --namespace fluidbg-system >/tmp/fluidbg-operator-rendered.yaml

e2e:
    KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh

test-env-up:
    kind create cluster --config testenv/kind-config.yaml
    kubectl create namespace fluidbg-system
    kubectl create namespace fluidbg-test
    helm repo add bitnami https://charts.bitnami.com/bitnami
    helm repo update
    helm upgrade --install rabbitmq bitnami/rabbitmq \
      -n fluidbg-system -f testenv/rabbitmq.yaml --wait
    helm upgrade --install postgres bitnami/postgresql \
      -n fluidbg-system -f testenv/postgres.yaml --wait

test-env-down:
    kind delete cluster --name fluidbg-dev

kind-load image:
    kind load docker-image {{image}} --name fluidbg-dev
