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
