use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec,
    Service, Volume, VolumeMount,
};
use kube::core::ObjectMeta;

use crate::crd::blue_green::InceptionPoint;
use crate::crd::inception_plugin::{Direction, InceptionPlugin, Topology};

pub struct ReconciledResources {
    pub config_maps: Vec<ConfigMap>,
    pub deployments: Vec<Deployment>,
    pub services: Vec<Service>,
    pub sidecar_containers: Vec<Container>,
    pub blue_env_injections: Vec<EnvVar>,
    pub test_env_injections: Vec<EnvVar>,
}

pub fn reconcile_inception_point(
    plugin: &InceptionPlugin,
    ip: &InceptionPoint,
    namespace: &str,
    operator_url: &str,
    test_container_url: &str,
    _blue_deployment_name: &str,
) -> Result<ReconciledResources, String> {
    crate::validation::validate_mode_direction(&ip.mode, &ip.directions)?;

    for dir in &ip.directions {
        if !plugin.spec.directions.contains(dir) {
            return Err(format!(
                "inception point '{}' requests direction {:?} but plugin '{}' only supports {:?}",
                ip.name,
                dir,
                plugin.metadata.name.as_deref().unwrap_or(""),
                plugin.spec.directions
            ));
        }
    }

    if !plugin.spec.modes.contains(&ip.mode) {
        return Err(format!(
            "inception point '{}' requests mode {:?} but plugin '{}' only supports {:?}",
            ip.name,
            ip.mode,
            plugin.metadata.name.as_deref().unwrap_or(""),
            plugin.spec.modes
        ));
    }

    crate::plugins::schema::validate_config_against_schema(&ip.config, &plugin.spec.config_schema)
        .map_err(|errs| format!("config validation failed: {:?}", errs))?;

    let config_name = format!("fluidbg-config-{}", ip.name);
    let directions_str = ip
        .directions
        .iter()
        .map(|d| match d {
            Direction::Ingress => "ingress",
            Direction::Egress => "egress",
        })
        .collect::<Vec<_>>()
        .join(",");

    let mode_str = match ip.mode {
        crate::crd::inception_plugin::PluginMode::Trigger => "trigger",
        crate::crd::inception_plugin::PluginMode::PassthroughDuplicate => "passthrough-duplicate",
        crate::crd::inception_plugin::PluginMode::RerouteMock => "reroute-mock",
        crate::crd::inception_plugin::PluginMode::Write => "write",
    };

    let config_data = if let Some(template) = &plugin.spec.config_template {
        if !template.is_empty() {
            let rendered = crate::plugins::template::render_config_template(template, &ip.config)?;
            BTreeMap::from([("config.yaml".to_string(), rendered)])
        } else {
            BTreeMap::from([(
                "config.yaml".to_string(),
                serde_yaml::to_string(&ip.config).unwrap_or_default(),
            )])
        }
    } else {
        BTreeMap::from([(
            "config.yaml".to_string(),
            serde_yaml::to_string(&ip.config).unwrap_or_default(),
        )])
    };

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(config_name.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        data: Some(config_data),
        ..Default::default()
    };

    let env_vars = vec![
        EnvVar {
            name: "FLUIDBG_OPERATOR_URL".to_string(),
            value: Some(operator_url.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_TEST_CONTAINER_URL".to_string(),
            value: Some(test_container_url.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_INCEPTION_POINT".to_string(),
            value: Some(ip.name.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_MODE".to_string(),
            value: Some(mode_str.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_DIRECTIONS".to_string(),
            value: Some(directions_str),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_CONFIG_PATH".to_string(),
            value: Some("/etc/fluidbg/config.yaml".to_string()),
            ..Default::default()
        },
    ];

    let volume_mounts = plugin
        .spec
        .container
        .volume_mounts
        .iter()
        .map(|vm| VolumeMount {
            name: vm.name.clone(),
            mount_path: vm.mount_path.clone(),
            read_only: vm.read_only,
            ..Default::default()
        })
        .collect::<Vec<_>>();

    let container_ports = plugin
        .spec
        .container
        .ports
        .iter()
        .map(|p| ContainerPort {
            name: Some(p.name.clone()),
            container_port: p.container_port,
            ..Default::default()
        })
        .collect::<Vec<_>>();

    let plugin_container = Container {
        name: format!("fluidbg-{}", ip.name),
        image: Some(plugin.spec.image.clone()),
        env: Some(env_vars),
        ports: if container_ports.is_empty() {
            None
        } else {
            Some(container_ports)
        },
        volume_mounts: if volume_mounts.is_empty() {
            None
        } else {
            Some(volume_mounts)
        },
        ..Default::default()
    };

    let volumes = vec![Volume {
        name: "plugin-config".to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: config_name.clone(),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let mut blue_env_injections = Vec::new();
    let mut test_env_injections = Vec::new();

    if let Some(injects) = &plugin.spec.injects {
        if let Some(blue_inj) = &injects.blue_container {
            for env_inj in &blue_inj.env {
                let env_var_name = ip
                    .config
                    .get(&env_inj.name_from_config)
                    .and_then(|v| v.as_str())
                    .unwrap_or(&env_inj.name_from_config);

                let value = crate::plugins::template::render_config_template(
                    &env_inj.value_template,
                    &ip.config,
                )
                .unwrap_or_else(|_| env_inj.value_template.clone());

                blue_env_injections.push(EnvVar {
                    name: env_var_name.to_string(),
                    value: Some(value),
                    ..Default::default()
                });
            }
        }
        if let Some(test_inj) = &injects.test_container {
            for env_inj in &test_inj.env {
                let env_var_name = ip
                    .config
                    .get(&env_inj.name_from_config)
                    .and_then(|v| v.as_str())
                    .unwrap_or(&env_inj.name_from_config);

                let value = crate::plugins::template::render_config_template(
                    &env_inj.value_template,
                    &ip.config,
                )
                .unwrap_or_else(|_| env_inj.value_template.clone());

                test_env_injections.push(EnvVar {
                    name: env_var_name.to_string(),
                    value: Some(value),
                    ..Default::default()
                });
            }
        }
    }

    match plugin.spec.topology {
        Topology::SidecarBlue => {
            let mut sidecar = plugin_container.clone();
            sidecar.volume_mounts = Some(
                vec![VolumeMount {
                    name: "plugin-config".to_string(),
                    mount_path: "/etc/fluidbg".to_string(),
                    read_only: Some(true),
                    ..Default::default()
                }]
                .into_iter()
                .chain(sidecar.volume_mounts.unwrap_or_default())
                .collect(),
            );
            Ok(ReconciledResources {
                config_maps: vec![cm],
                deployments: vec![],
                services: vec![],
                sidecar_containers: vec![sidecar],
                blue_env_injections,
                test_env_injections,
            })
        }
        Topology::Standalone => {
            let deployment_name = format!("fluidbg-{}", ip.name);
            let labels = BTreeMap::from([
                ("app".to_string(), deployment_name.clone()),
                ("fluidbg.io/inception-point".to_string(), ip.name.clone()),
            ]);

            let pod_spec = PodSpec {
                containers: vec![{
                    let mut c = plugin_container;
                    c.volume_mounts = Some(
                        vec![VolumeMount {
                            name: "plugin-config".to_string(),
                            mount_path: "/etc/fluidbg".to_string(),
                            read_only: Some(true),
                            ..Default::default()
                        }]
                        .into_iter()
                        .chain(c.volume_mounts.unwrap_or_default())
                        .collect(),
                    );
                    c
                }],
                volumes: Some(volumes),
                ..Default::default()
            };

            let deployment = Deployment {
                metadata: ObjectMeta {
                    name: Some(deployment_name.clone()),
                    namespace: Some(namespace.to_string()),
                    labels: Some(labels.clone()),
                    ..Default::default()
                },
                spec: Some(k8s_openapi::api::apps::v1::DeploymentSpec {
                    selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                        match_labels: Some(labels.clone()),
                        ..Default::default()
                    },
                    template: PodTemplateSpec {
                        metadata: Some(ObjectMeta {
                            labels: Some(labels.clone()),
                            ..Default::default()
                        }),
                        spec: Some(pod_spec),
                    },
                    ..Default::default()
                }),
                ..Default::default()
            };

            let service = Service {
                metadata: ObjectMeta {
                    name: Some(format!("fluidbg-{}-svc", ip.name)),
                    namespace: Some(namespace.to_string()),
                    labels: Some(labels),
                    ..Default::default()
                },
                spec: Some(k8s_openapi::api::core::v1::ServiceSpec {
                    selector: Some([("app".to_string(), deployment_name)].into_iter().collect()),
                    ports: Some(
                        plugin
                            .spec
                            .container
                            .ports
                            .iter()
                            .map(|p| k8s_openapi::api::core::v1::ServicePort {
                                name: Some(p.name.clone()),
                                port: p.container_port,
                                target_port: Some(
                                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                                        p.container_port,
                                    ),
                                ),
                                ..Default::default()
                            })
                            .collect(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            };

            Ok(ReconciledResources {
                config_maps: vec![cm],
                deployments: vec![deployment],
                services: vec![service],
                sidecar_containers: vec![],
                blue_env_injections,
                test_env_injections,
            })
        }
        Topology::SidecarTest => {
            let mut sidecar = plugin_container.clone();
            sidecar.volume_mounts = Some(
                vec![VolumeMount {
                    name: "plugin-config".to_string(),
                    mount_path: "/etc/fluidbg".to_string(),
                    read_only: Some(true),
                    ..Default::default()
                }]
                .into_iter()
                .chain(sidecar.volume_mounts.unwrap_or_default())
                .collect(),
            );
            Ok(ReconciledResources {
                config_maps: vec![cm],
                deployments: vec![],
                services: vec![],
                sidecar_containers: vec![sidecar],
                blue_env_injections,
                test_env_injections,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::crd::inception_plugin::{
        ContainerPort, Direction, InceptionPlugin, InceptionPluginSpec, Injects, PluginMode,
        Topology, VolumeMount,
    };

    use super::*;

    fn make_http_proxy_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "http-proxy",
            InceptionPluginSpec {
                description: "HTTP proxy sidecar".to_string(),
                image: "fluidbg/http-proxy:v0.1.0".to_string(),
                topology: Topology::SidecarBlue,
                modes: vec![
                    PluginMode::Trigger,
                    PluginMode::PassthroughDuplicate,
                    PluginMode::RerouteMock,
                ],
                directions: vec![Direction::Ingress, Direction::Egress],
                field_namespaces: vec!["http".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "required": ["proxyPort", "realEndpoint", "envVarName"],
                    "properties": {
                        "proxyPort": { "type": "integer" },
                        "realEndpoint": { "type": "string" },
                        "envVarName": { "type": "string" },
                        "testId": { "type": "object" },
                        "match": { "type": "array" },
                        "filters": { "type": "array" }
                    }
                }),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![ContainerPort {
                        name: "proxy".to_string(),
                        container_port: 8080,
                    }],
                    volume_mounts: vec![VolumeMount {
                        name: "plugin-config".to_string(),
                        mount_path: "/etc/fluidbg".to_string(),
                        read_only: Some(true),
                    }],
                },
                injects: Some(Injects {
                    blue_container: Some(crate::crd::inception_plugin::ContainerInjection {
                        env: vec![crate::crd::inception_plugin::EnvInjection {
                            name_from_config: "envVarName".to_string(),
                            value_template: "http://localhost:{{proxyPort}}".to_string(),
                        }],
                    }),
                    test_container: None,
                }),
            },
        )
    }

    fn make_rabbitmq_duplicator_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "rabbitmq-duplicator",
            InceptionPluginSpec {
                description: "RabbitMQ duplicator".to_string(),
                image: "fluidbg/rabbitmq-duplicator:v0.1.0".to_string(),
                topology: Topology::Standalone,
                modes: vec![PluginMode::Trigger, PluginMode::PassthroughDuplicate],
                directions: vec![Direction::Ingress, Direction::Egress],
                field_namespaces: vec!["queue".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "required": ["sourceQueue", "shadowQueue"],
                    "properties": {
                        "sourceQueue": { "type": "string" },
                        "shadowQueue": { "type": "string" },
                        "testId": { "type": "object" },
                        "match": { "type": "array" },
                        "writeEnvVar": { "type": "string" }
                    }
                }),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![ContainerPort {
                        name: "health".to_string(),
                        container_port: 9090,
                    }],
                    volume_mounts: vec![],
                },
                injects: None,
            },
        )
    }

    fn make_fake_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "fake-plugin",
            InceptionPluginSpec {
                description: "A test plugin".to_string(),
                image: "example.com/fake-plugin:1".to_string(),
                topology: Topology::Standalone,
                modes: vec![PluginMode::Trigger],
                directions: vec![Direction::Ingress],
                field_namespaces: vec!["custom".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "required": ["target"],
                    "properties": {
                        "target": { "type": "string" }
                    }
                }),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![ContainerPort {
                        name: "http".to_string(),
                        container_port: 8080,
                    }],
                    volume_mounts: vec![VolumeMount {
                        name: "plugin-config".to_string(),
                        mount_path: "/etc/fluidbg".to_string(),
                        read_only: Some(true),
                    }],
                },
                injects: None,
            },
        )
    }

    fn make_inception_point(
        name: &str,
        mode: PluginMode,
        directions: Vec<Direction>,
        config: serde_json::Value,
    ) -> InceptionPoint {
        InceptionPoint {
            name: name.to_string(),
            directions,
            mode,
            plugin_ref: crate::crd::blue_green::PluginRef {
                name: "test".to_string(),
            },
            config,
            notify_tests: vec![],
            timeout: Some("60s".to_string()),
        }
    }

    #[test]
    fn sidecar_blue_produces_sidecar_and_configmap() {
        let plugin = make_http_proxy_plugin();
        let ip = make_inception_point(
            "payment-calls",
            PluginMode::PassthroughDuplicate,
            vec![Direction::Egress],
            serde_json::json!({
                "proxyPort": 8081,
                "realEndpoint": "https://payment.internal/v1",
                "envVarName": "PAYMENT_SERVICE_URL",
                "testId": {"field": "http.body", "jsonPath": "$.orderId"},
                "filters": [{"match": [{"field": "http.path", "matches": "^/v1/charge$"}], "notifyPath": "/observe/{testId}/payment-charge", "payload": "both"}]
            }),
        );
        let resources = reconcile_inception_point(
            &plugin,
            &ip,
            "production",
            "http://operator:8090",
            "http://test-container:8080",
            "order-processor-blue",
        )
        .unwrap();

        assert_eq!(resources.config_maps.len(), 1);
        assert_eq!(resources.sidecar_containers.len(), 1);
        assert_eq!(resources.deployments.len(), 0);
        assert_eq!(resources.services.len(), 0);
        assert!(!resources.blue_env_injections.is_empty());
        assert_eq!(resources.blue_env_injections[0].name, "PAYMENT_SERVICE_URL");
        assert_eq!(
            resources.blue_env_injections[0].value,
            Some("http://localhost:8081".to_string())
        );
    }

    #[test]
    fn standalone_produces_deployment_and_service() {
        let plugin = make_rabbitmq_duplicator_plugin();
        let ip = make_inception_point(
            "incoming-orders",
            PluginMode::Trigger,
            vec![Direction::Ingress],
            serde_json::json!({
                "sourceQueue": "orders",
                "shadowQueue": "orders-blue",
                "testId": {"field": "queue.body", "jsonPath": "$.orderId"},
                "match": [{"field": "queue.body", "jsonPath": "$.type", "matches": "^order$"}],
                "notifyPath": "/trigger"
            }),
        );
        let resources = reconcile_inception_point(
            &plugin,
            &ip,
            "production",
            "http://operator:8090",
            "http://test-container:8080",
            "order-processor-blue",
        )
        .unwrap();

        assert_eq!(resources.config_maps.len(), 1);
        assert_eq!(resources.deployments.len(), 1);
        assert_eq!(resources.services.len(), 1);
        assert_eq!(resources.sidecar_containers.len(), 0);

        let deploy = &resources.deployments[0];
        assert_eq!(
            deploy.metadata.name.as_deref(),
            Some("fluidbg-incoming-orders")
        );
        assert_eq!(
            deploy
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap()
                .containers
                .len(),
            1
        );
        assert_eq!(
            deploy
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap()
                .containers[0]
                .image
                .as_deref(),
            Some("fluidbg/rabbitmq-duplicator:v0.1.0")
        );
    }

    #[test]
    fn adding_plugin_requires_no_code_changes() {
        let plugin = make_fake_plugin();
        let ip = make_inception_point(
            "custom-point",
            PluginMode::Trigger,
            vec![Direction::Ingress],
            serde_json::json!({"target": "custom-queue"}),
        );
        let resources = reconcile_inception_point(
            &plugin,
            &ip,
            "test-ns",
            "http://operator:8090",
            "http://tc:8080",
            "blue",
        )
        .unwrap();

        assert_eq!(resources.deployments.len(), 1);
        assert_eq!(resources.services.len(), 1);
        assert_eq!(resources.config_maps.len(), 1);

        let deploy = &resources.deployments[0];
        let container = &deploy
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0];
        assert_eq!(
            container.image.as_deref(),
            Some("example.com/fake-plugin:1")
        );
        assert!(
            container
                .env
                .as_ref()
                .unwrap()
                .iter()
                .any(|e| e.name == "FLUIDBG_MODE" && e.value == Some("trigger".to_string()))
        );
    }

    #[test]
    fn invalid_mode_direction_rejected() {
        let plugin = make_http_proxy_plugin();
        let ip = make_inception_point(
            "bad-point",
            PluginMode::RerouteMock,
            vec![Direction::Ingress],
            serde_json::json!({
                "proxyPort": 8082,
                "realEndpoint": "http://upstream",
                "envVarName": "UPSTREAM_URL"
            }),
        );
        assert!(
            reconcile_inception_point(
                &plugin,
                &ip,
                "ns",
                "http://op:8090",
                "http://tc:8080",
                "blue"
            )
            .is_err()
        );
    }

    #[test]
    fn unsupported_direction_rejected() {
        let ip = make_inception_point(
            "bad-dir",
            PluginMode::Trigger,
            vec![Direction::Ingress],
            serde_json::json!({"sourceQueue": "q", "shadowQueue": "q-blue"}),
        );
        let egress_only_plugin = InceptionPlugin::new(
            "egress-only",
            InceptionPluginSpec {
                description: "Egress only".to_string(),
                image: "test:v1".to_string(),
                topology: Topology::Standalone,
                modes: vec![PluginMode::PassthroughDuplicate],
                directions: vec![Direction::Egress],
                field_namespaces: vec!["queue".to_string()],
                config_schema: serde_json::json!({"type": "object"}),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![],
                    volume_mounts: vec![],
                },
                injects: None,
            },
        );
        assert!(
            reconcile_inception_point(
                &egress_only_plugin,
                &ip,
                "ns",
                "http://op:8090",
                "http://tc:8080",
                "blue"
            )
            .is_err()
        );
    }
}
