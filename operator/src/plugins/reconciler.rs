use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec,
    Service, Volume, VolumeMount,
};
use kube::core::ObjectMeta;

use crate::crd::blue_green::InceptionPoint;
use crate::crd::inception_plugin::{InceptionPlugin, PluginRole, Topology};

pub struct ReconciledResources {
    pub config_maps: Vec<ConfigMap>,
    pub deployments: Vec<Deployment>,
    pub services: Vec<Service>,
    pub sidecar_containers: Vec<Container>,
    pub green_env_injections: Vec<EnvVar>,
    pub blue_env_injections: Vec<EnvVar>,
    pub test_env_injections: Vec<EnvVar>,
}

pub struct ContainerEnvInjections {
    pub green: Vec<EnvVar>,
    pub blue: Vec<EnvVar>,
    pub test: Vec<EnvVar>,
}

pub fn reconcile_inception_point(
    plugin: &InceptionPlugin,
    ip: &InceptionPoint,
    namespace: &str,
    operator_url: &str,
    test_container_url: &str,
    _blue_deployment_name: &str,
    blue_green_ref: &str,
) -> Result<ReconciledResources, String> {
    crate::validation::validate_roles(&plugin.spec.supported_roles, &ip.roles)?;

    crate::plugins::schema::validate_config_against_schema(&ip.config, &plugin.spec.config_schema)
        .map_err(|errs| format!("config validation failed: {:?}", errs))?;

    let config_name = format!("fluidbg-config-{}", ip.name);
    let role_str = ip
        .roles
        .iter()
        .map(plugin_role_name)
        .collect::<Vec<_>>()
        .join(",");

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
            name: "FLUIDBG_BLUE_GREEN_REF".to_string(),
            value: Some(blue_green_ref.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_ACTIVE_ROLES".to_string(),
            value: Some(role_str.to_string()),
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

    let env_injections = render_container_env_injections(plugin, ip, false);

    match plugin.spec.topology {
        Topology::SidecarBlue => {
            let mut sidecar = plugin_container.clone();
            ensure_config_mount(&mut sidecar);
            Ok(ReconciledResources {
                config_maps: vec![cm],
                deployments: vec![],
                services: vec![],
                sidecar_containers: vec![sidecar],
                green_env_injections: env_injections.green,
                blue_env_injections: env_injections.blue,
                test_env_injections: env_injections.test,
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
                    ensure_config_mount(&mut c);
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
                green_env_injections: env_injections.green,
                blue_env_injections: env_injections.blue,
                test_env_injections: env_injections.test,
            })
        }
        Topology::SidecarTest => {
            let mut sidecar = plugin_container.clone();
            ensure_config_mount(&mut sidecar);
            Ok(ReconciledResources {
                config_maps: vec![cm],
                deployments: vec![],
                services: vec![],
                sidecar_containers: vec![sidecar],
                green_env_injections: env_injections.green,
                blue_env_injections: env_injections.blue,
                test_env_injections: env_injections.test,
            })
        }
    }
}

fn ensure_config_mount(container: &mut Container) {
    let mounts = container.volume_mounts.get_or_insert_with(Vec::new);
    if mounts.iter().any(|mount| mount.mount_path == "/etc/fluidbg") {
        return;
    }
    mounts.push(VolumeMount {
        name: "plugin-config".to_string(),
        mount_path: "/etc/fluidbg".to_string(),
        read_only: Some(true),
        ..Default::default()
    });
}

fn plugin_role_name(role: &PluginRole) -> &'static str {
    match role {
        PluginRole::Splitter => "splitter",
        PluginRole::Combiner => "combiner",
        PluginRole::Observer => "observer",
        PluginRole::Mock => "mock",
        PluginRole::Writer => "writer",
        PluginRole::Sink => "sink",
    }
}

pub fn render_container_env_injections(
    plugin: &InceptionPlugin,
    ip: &InceptionPoint,
    restore_values: bool,
) -> ContainerEnvInjections {
    let Some(injects) = &plugin.spec.injects else {
        return ContainerEnvInjections {
            green: Vec::new(),
            blue: Vec::new(),
            test: Vec::new(),
        };
    };

    ContainerEnvInjections {
        green: render_env_injection_set(
            injects.green_container.as_ref(),
            &ip.config,
            restore_values,
        ),
        blue: render_env_injection_set(injects.blue_container.as_ref(), &ip.config, restore_values),
        test: render_env_injection_set(injects.test_container.as_ref(), &ip.config, restore_values),
    }
}

fn render_env_injection_set(
    injection: Option<&crate::crd::inception_plugin::ContainerInjection>,
    config: &serde_json::Value,
    restore_values: bool,
) -> Vec<EnvVar> {
    let Some(injection) = injection else {
        return Vec::new();
    };

    injection
        .env
        .iter()
        .map(|env_inj| {
            let env_var_name = config
                .get(&env_inj.name_from_config)
                .and_then(|v| v.as_str())
                .unwrap_or(&env_inj.name_from_config);
            let template = if restore_values {
                env_inj
                    .restore_value_template
                    .as_ref()
                    .unwrap_or(&env_inj.value_template)
            } else {
                &env_inj.value_template
            };
            let value = crate::plugins::template::render_config_template(template, config)
                .unwrap_or_else(|_| template.clone());

            EnvVar {
                name: env_var_name.to_string(),
                value: Some(value),
                ..Default::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::crd::inception_plugin::{
        ContainerPort, InceptionPlugin, InceptionPluginSpec, Injects, PluginFeatures, PluginRole,
        Topology, VolumeMount,
    };

    use super::*;

    fn make_http_proxy_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "http-proxy",
            InceptionPluginSpec {
                description: "HTTP proxy sidecar".to_string(),
                image: "fluidbg/http-proxy:v0.1.0".to_string(),
                supported_roles: vec![PluginRole::Observer, PluginRole::Mock],
                topology: Topology::SidecarBlue,
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
                lifecycle: None,
                injects: Some(Injects {
                    green_container: None,
                    blue_container: Some(crate::crd::inception_plugin::ContainerInjection {
                        env: vec![crate::crd::inception_plugin::EnvInjection {
                            name_from_config: "envVarName".to_string(),
                            value_template: "http://localhost:{{proxyPort}}".to_string(),
                            restore_value_template: None,
                        }],
                    }),
                    test_container: None,
                }),
                features: None,
            },
        )
    }

    fn make_rabbitmq_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "rabbitmq",
            InceptionPluginSpec {
                description: "RabbitMQ transport plugin".to_string(),
                image: "fluidbg/rabbitmq:v0.1.0".to_string(),
                supported_roles: vec![
                    PluginRole::Splitter,
                    PluginRole::Observer,
                    PluginRole::Writer,
                    PluginRole::Sink,
                    PluginRole::Combiner,
                ],
                topology: Topology::Standalone,
                field_namespaces: vec!["queue".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "required": ["inputQueue", "greenInputQueue", "blueInputQueue"],
                    "properties": {
                        "inputQueue": { "type": "string" },
                        "greenInputQueue": { "type": "string" },
                        "blueInputQueue": { "type": "string" },
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
                lifecycle: None,
                injects: None,
                features: Some(PluginFeatures {
                    supports_progressive_shifting: true,
                }),
            },
        )
    }

    fn make_fake_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "fake-plugin",
            InceptionPluginSpec {
                description: "A test plugin".to_string(),
                image: "example.com/fake-plugin:1".to_string(),
                supported_roles: vec![PluginRole::Observer],
                topology: Topology::Standalone,
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
                lifecycle: None,
                injects: None,
                features: None,
            },
        )
    }

    fn make_inception_point(
        name: &str,
        roles: Vec<PluginRole>,
        config: serde_json::Value,
    ) -> InceptionPoint {
        InceptionPoint {
            name: name.to_string(),
            plugin_ref: crate::crd::blue_green::PluginRef {
                name: "test".to_string(),
            },
            roles,
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
            vec![PluginRole::Observer],
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
            "order-processor-bg",
        )
        .unwrap();

        assert_eq!(resources.config_maps.len(), 1);
        assert_eq!(resources.sidecar_containers.len(), 1);
        assert_eq!(resources.deployments.len(), 0);
        assert_eq!(resources.services.len(), 0);
        assert!(resources.green_env_injections.is_empty());
        assert!(!resources.blue_env_injections.is_empty());
        assert_eq!(resources.blue_env_injections[0].name, "PAYMENT_SERVICE_URL");
        assert_eq!(
            resources.blue_env_injections[0].value,
            Some("http://localhost:8081".to_string())
        );
    }

    #[test]
    fn standalone_produces_deployment_and_service() {
        let plugin = make_rabbitmq_plugin();
        let ip = make_inception_point(
            "incoming-orders",
            vec![PluginRole::Splitter, PluginRole::Observer],
            serde_json::json!({
                "inputQueue": "orders",
                "greenInputQueue": "orders-green",
                "blueInputQueue": "orders-blue",
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
            "order-processor-bg",
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
            Some("fluidbg/rabbitmq:v0.1.0")
        );
    }

    #[test]
    fn adding_plugin_requires_no_code_changes() {
        let plugin = make_fake_plugin();
        let ip = make_inception_point(
            "custom-point",
            vec![PluginRole::Observer],
            serde_json::json!({"target": "custom-queue"}),
        );
        let resources = reconcile_inception_point(
            &plugin,
            &ip,
            "test-ns",
            "http://operator:8090",
            "http://tc:8080",
            "blue",
            "order-processor-bg",
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
            container.env.as_ref().unwrap().iter().any(
                |e| e.name == "FLUIDBG_ACTIVE_ROLES" && e.value == Some("observer".to_string())
            )
        );
    }

    #[test]
    fn unsupported_role_rejected() {
        let plugin = make_http_proxy_plugin();
        let ip = make_inception_point(
            "bad-point",
            vec![PluginRole::Splitter],
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
                "blue",
                "order-processor-bg"
            )
            .is_err()
        );
    }

    #[test]
    fn sink_role_supported() {
        let ip = make_inception_point(
            "sink",
            vec![PluginRole::Sink],
            serde_json::json!({"target": "q"}),
        );
        let sink_plugin = InceptionPlugin::new(
            "sink",
            InceptionPluginSpec {
                description: "Sink".to_string(),
                image: "test:v1".to_string(),
                supported_roles: vec![PluginRole::Sink],
                topology: Topology::Standalone,
                field_namespaces: vec!["queue".to_string()],
                config_schema: serde_json::json!({"type": "object"}),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![],
                    volume_mounts: vec![],
                },
                lifecycle: None,
                injects: None,
                features: None,
            },
        );
        assert!(
            reconcile_inception_point(
                &sink_plugin,
                &ip,
                "ns",
                "http://op:8090",
                "http://tc:8080",
                "blue",
                "order-processor-bg"
            )
            .is_ok()
        );
    }
}
