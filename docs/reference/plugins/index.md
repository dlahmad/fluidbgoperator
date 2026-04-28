---
layout: page
title: Built-In Plugins
---

# Built-In Plugins

This section contains the formal references for built-in plugin behavior. The
shared operator/plugin protocol is documented in
[Plugin Interface](../plugin-interface.md); the pages below describe
plugin-specific roles, configuration, state machines, failure behavior, and
drain semantics.

## Common Template

Each plugin page uses the same structure:

- Identity and supported topology.
- Configuration reference.
- Role behavior.
- Runtime state machine.
- Failure behavior.
- Drain, cleanup, and loss-prevention semantics.
- Security and privilege boundaries.

## Plugins

| Plugin | Purpose | Reference |
|---|---|---|
| `rabbitmq` | Queue fanout, weighted splitting, output combining, observation, writing, and consumption for RabbitMQ. | [RabbitMQ Plugin](rabbitmq.md) |
| `azure-servicebus` | Queue fanout, weighted splitting, output combining, observation, writing, and consumption for Azure Service Bus. | [Azure Service Bus Plugin](azure-servicebus.md) |
| `http` | HTTP proxy/splitter, observer/mock, and writer behavior. | [HTTP Plugin](http.md) |
