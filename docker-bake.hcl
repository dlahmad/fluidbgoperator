variable "REGISTRY" {
  default = "fluidbg"
}

variable "TAG" {
  default = "dev"
}

group "default" {
  targets = ["fbg-operator", "fbg-plugin-http", "fbg-plugin-rabbitmq"]
}

target "fbg-operator" {
  context = "."
  dockerfile = "Dockerfile"
  tags = ["${REGISTRY}/fbg-operator:${TAG}"]
}

target "fbg-plugin-http" {
  context = "."
  dockerfile = "plugins/http/Dockerfile"
  tags = ["${REGISTRY}/fbg-plugin-http:${TAG}"]
}

target "fbg-plugin-rabbitmq" {
  context = "."
  dockerfile = "plugins/rabbitmq/Dockerfile"
  tags = ["${REGISTRY}/fbg-plugin-rabbitmq:${TAG}"]
}
