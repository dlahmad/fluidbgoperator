variable "REGISTRY" {
  default = "fluidbg"
}

variable "TAG" {
  default = "dev"
}

group "default" {
  targets = ["operator", "http", "rabbitmq"]
}

target "operator" {
  context = "."
  dockerfile = "Dockerfile"
  tags = ["${REGISTRY}/operator:${TAG}"]
}

target "http" {
  context = "."
  dockerfile = "plugins/http/Dockerfile"
  tags = ["${REGISTRY}/http:${TAG}"]
}

target "rabbitmq" {
  context = "."
  dockerfile = "plugins/rabbitmq/Dockerfile"
  tags = ["${REGISTRY}/rabbitmq:${TAG}"]
}
