use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec,
    Service, Volume, VolumeMount,
};
use kube::core::ObjectMeta;
use sha2::{Digest, Sha256};

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

fn sanitize_dns_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_dash = false;
    for ch in value.chars() {
        let lowered = ch.to_ascii_lowercase();
        let valid = lowered.is_ascii_alphanumeric();
        if valid {
            out.push(lowered);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn stable_name_suffix(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .flat_map(|byte| {
            let hi = byte >> 4;
            let lo = byte & 0x0f;
            [
                char::from_digit(hi.into(), 16),
                char::from_digit(lo.into(), 16),
            ]
        })
        .flatten()
        .take(12)
        .collect()
}

fn truncate_label(value: &str, max_len: usize) -> String {
    value.chars().take(max_len).collect()
}

pub fn inception_instance_base_name(blue_green_ref: &str, inception_point: &str) -> String {
    let ip = sanitize_dns_label(inception_point);
    let ip = if ip.is_empty() { "ip".to_string() } else { ip };
    let suffix = stable_name_suffix(&[blue_green_ref, inception_point]);
    let max_ip_len = 63usize.saturating_sub("fluidbg--".len() + suffix.len());
    let ip = truncate_label(&ip, max_ip_len);
    format!("fluidbg-{}-{}", ip, suffix)
}

pub fn inception_service_name(blue_green_ref: &str, inception_point: &str) -> String {
    let ip = sanitize_dns_label(inception_point);
    let ip = if ip.is_empty() { "ip".to_string() } else { ip };
    let suffix = stable_name_suffix(&[blue_green_ref, inception_point]);
    let max_ip_len = 63usize.saturating_sub("fluidbg---svc".len() + suffix.len());
    let ip = truncate_label(&ip, max_ip_len);
    format!("fluidbg-{}-{}-svc", ip, suffix)
}

pub fn inception_config_map_name(blue_green_ref: &str, inception_point: &str) -> String {
    let ip = sanitize_dns_label(inception_point);
    let ip = if ip.is_empty() { "ip".to_string() } else { ip };
    let suffix = stable_name_suffix(&[blue_green_ref, inception_point]);
    let prefix = "fluidbg-config-";
    let max_ip_len = 63usize.saturating_sub(prefix.len() + 1 + suffix.len());
    format!("{}{}-{}", prefix, truncate_label(&ip, max_ip_len), suffix)
}

pub fn reconcile_inception_point(
    plugin: &InceptionPlugin,
    ip: &InceptionPoint,
    namespace: &str,
    operator_url: &str,
    test_container_url: &str,
    test_data_verify_path: Option<&str>,
    _blue_deployment_name: &str,
    blue_green_ref: &str,
) -> Result<ReconciledResources, String> {
    crate::validation::validate_roles(&plugin.spec.supported_roles, &ip.roles)?;

    crate::plugins::schema::validate_config_against_schema(&ip.config, &plugin.spec.config_schema)
        .map_err(|errs| format!("config validation failed: {:?}", errs))?;

    let config_name = inception_config_map_name(blue_green_ref, &ip.name);
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
            labels: Some(BTreeMap::from([
                ("fluidbg.io/inception-point".to_string(), ip.name.clone()),
                (
                    "fluidbg.io/blue-green-ref".to_string(),
                    blue_green_ref.to_string(),
                ),
            ])),
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
            name: "FLUIDBG_TESTCASE_REGISTRATION_URL".to_string(),
            value: Some(format!("{}/testcases", operator_url.trim_end_matches('/'))),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_TEST_CONTAINER_URL".to_string(),
            value: Some(test_container_url.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE".to_string(),
            value: test_data_verify_path.map(|value| value.to_string()),
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
        name: inception_instance_base_name(blue_green_ref, &ip.name),
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

    let deployment_name = inception_instance_base_name(blue_green_ref, &ip.name);
    let service_name = inception_service_name(blue_green_ref, &ip.name);
    let template_context = plugin_template_context(ip, namespace, blue_green_ref);
    let env_injections = render_container_env_injections(plugin, &template_context, false);

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
            let labels = BTreeMap::from([
                ("app".to_string(), deployment_name.clone()),
                ("fluidbg.io/inception-point".to_string(), ip.name.clone()),
                (
                    "fluidbg.io/blue-green-ref".to_string(),
                    blue_green_ref.to_string(),
                ),
            ]);

            let pod_spec = PodSpec {
                containers: vec![{
                    let mut c = plugin_container;
                    ensure_config_mount(&mut c);
                    c
                }],
                volumes: Some(volumes),
                termination_grace_period_seconds: Some(1),
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
                    name: Some(service_name),
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
    if mounts
        .iter()
        .any(|mount| mount.mount_path == "/etc/fluidbg")
    {
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
        PluginRole::Duplicator => "duplicator",
        PluginRole::Splitter => "splitter",
        PluginRole::Combiner => "combiner",
        PluginRole::Observer => "observer",
        PluginRole::Mock => "mock",
        PluginRole::Writer => "writer",
        PluginRole::Consumer => "consumer",
    }
}

pub fn render_container_env_injections(
    plugin: &InceptionPlugin,
    template_context: &serde_json::Value,
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
            template_context,
            restore_values,
        ),
        blue: render_env_injection_set(
            injects.blue_container.as_ref(),
            template_context,
            restore_values,
        ),
        test: render_env_injection_set(
            injects.test_container.as_ref(),
            template_context,
            restore_values,
        ),
    }
}

pub fn plugin_template_context(
    ip: &InceptionPoint,
    namespace: &str,
    blue_green_ref: &str,
) -> serde_json::Value {
    let mut context = ip.config.clone();
    let serde_json::Value::Object(map) = &mut context else {
        return serde_json::json!({
            "inceptionPoint": ip.name,
            "blueGreenRef": blue_green_ref,
            "namespace": namespace,
            "pluginDeploymentName": inception_instance_base_name(blue_green_ref, &ip.name),
            "pluginServiceName": inception_service_name(blue_green_ref, &ip.name),
        });
    };
    map.insert(
        "inceptionPoint".to_string(),
        serde_json::Value::String(ip.name.clone()),
    );
    map.insert(
        "blueGreenRef".to_string(),
        serde_json::Value::String(blue_green_ref.to_string()),
    );
    map.insert(
        "namespace".to_string(),
        serde_json::Value::String(namespace.to_string()),
    );
    map.insert(
        "pluginDeploymentName".to_string(),
        serde_json::Value::String(inception_instance_base_name(blue_green_ref, &ip.name)),
    );
    map.insert(
        "pluginServiceName".to_string(),
        serde_json::Value::String(inception_service_name(blue_green_ref, &ip.name)),
    );
    context
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

    fn make_http_plugin() -> InceptionPlugin {
        InceptionPlugin::new(
            "http",
            InceptionPluginSpec {
                description: "HTTP transport plugin".to_string(),
                image: "fluidbg/http:v0.1.0".to_string(),
                supported_roles: vec![PluginRole::Observer, PluginRole::Mock, PluginRole::Writer],
                topology: Topology::Standalone,
                field_namespaces: vec!["http".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "port": { "type": "integer" },
                        "proxyPort": { "type": "integer" },
                        "realEndpoint": { "type": "string" },
                        "targetUrl": { "type": "string" },
                        "envVarName": { "type": "string" },
                        "writeEnvVar": { "type": "string" },
                        "testId": { "type": "object" },
                        "match": { "type": "array" },
                        "filters": { "type": "array" }
                    }
                }),
                config_template: None,
                container: crate::crd::inception_plugin::PluginContainer {
                    ports: vec![ContainerPort {
                        name: "http".to_string(),
                        container_port: 9090,
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
                            value_template: "http://{{pluginServiceName}}:9090".to_string(),
                            restore_value_template: None,
                        }],
                    }),
                    test_container: Some(crate::crd::inception_plugin::ContainerInjection {
                        env: vec![crate::crd::inception_plugin::EnvInjection {
                            name_from_config: "writeEnvVar".to_string(),
                            value_template: "http://{{pluginServiceName}}:9090".to_string(),
                            restore_value_template: None,
                        }],
                    }),
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
                    PluginRole::Duplicator,
                    PluginRole::Splitter,
                    PluginRole::Observer,
                    PluginRole::Writer,
                    PluginRole::Consumer,
                    PluginRole::Combiner,
                ],
                topology: Topology::Standalone,
                field_namespaces: vec!["queue".to_string()],
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "duplicator": {
                            "type": "object",
                            "properties": {
                                "inputQueue": { "type": "string" },
                                "greenInputQueue": { "type": "string" },
                                "blueInputQueue": { "type": "string" }
                            }
                        },
                        "splitter": {
                            "type": "object",
                            "properties": {
                                "inputQueue": { "type": "string" },
                                "greenInputQueue": { "type": "string" },
                                "blueInputQueue": { "type": "string" }
                            }
                        },
                        "observer": { "type": "object" },
                        "writer": { "type": "object" },
                        "consumer": { "type": "object" },
                        "combiner": { "type": "object" }
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
            drain: None,
            resources: vec![],
        }
    }

    #[test]
    fn inception_resource_names_are_stable_for_same_inputs() {
        assert_eq!(
            inception_instance_base_name("order-processor-bg", "incoming-orders"),
            inception_instance_base_name("order-processor-bg", "incoming-orders")
        );
        assert_eq!(
            inception_service_name("order-processor-bg", "incoming-orders"),
            inception_service_name("order-processor-bg", "incoming-orders")
        );
        assert_eq!(
            inception_config_map_name("order-processor-bg", "incoming-orders"),
            inception_config_map_name("order-processor-bg", "incoming-orders")
        );
    }

    #[test]
    fn inception_resource_names_are_scoped_by_rollout_name() {
        assert_ne!(
            inception_instance_base_name("rollout-a", "incoming-orders"),
            inception_instance_base_name("rollout-b", "incoming-orders")
        );
    }

    #[test]
    fn inception_resource_names_are_scoped_by_original_inception_point_name() {
        assert_ne!(
            inception_instance_base_name("rollout-a", "payment.calls"),
            inception_instance_base_name("rollout-a", "payment-calls")
        );
        assert_ne!(
            inception_service_name("rollout-a", "payment.calls"),
            inception_service_name("rollout-a", "payment-calls")
        );
        assert_ne!(
            inception_config_map_name("rollout-a", "payment.calls"),
            inception_config_map_name("rollout-a", "payment-calls")
        );
    }

    #[test]
    fn inception_resource_names_are_distinct_after_prefix_truncation() {
        let first = format!("{}-a", "x".repeat(200));
        let second = format!("{}-b", "x".repeat(200));

        assert_ne!(
            inception_instance_base_name("rollout-a", &first),
            inception_instance_base_name("rollout-a", &second)
        );
        assert_ne!(
            inception_service_name("rollout-a", &first),
            inception_service_name("rollout-a", &second)
        );
        assert_ne!(
            inception_config_map_name("rollout-a", &first),
            inception_config_map_name("rollout-a", &second)
        );
    }

    #[test]
    fn inception_resource_names_are_service_safe() {
        let names = [
            inception_instance_base_name("rollout-a", &"x".repeat(200)),
            inception_service_name("rollout-a", &"x".repeat(200)),
            inception_config_map_name("rollout-a", &"x".repeat(200)),
        ];

        for name in names {
            assert!(name.len() <= 63);
            assert!(
                name.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            );
        }
    }

    #[test]
    fn http_plugin_produces_standalone_service_and_env_injections() {
        let plugin = make_http_plugin();
        let ip = make_inception_point(
            "payment-calls",
            vec![PluginRole::Observer, PluginRole::Writer],
            serde_json::json!({
                "port": 9090,
                "realEndpoint": "https://payment.internal/v1",
                "targetUrl": "http://order-processor-blue:8080",
                "envVarName": "PAYMENT_SERVICE_URL",
                "writeEnvVar": "ORDER_WRITE_URL",
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
            Some("/result/{testId}"),
            "order-processor-blue",
            "order-processor-bg",
        )
        .unwrap();

        assert_eq!(resources.config_maps.len(), 1);
        assert_eq!(resources.sidecar_containers.len(), 0);
        assert_eq!(resources.deployments.len(), 1);
        assert_eq!(resources.services.len(), 1);
        assert!(resources.green_env_injections.is_empty());
        assert!(!resources.blue_env_injections.is_empty());
        assert!(!resources.test_env_injections.is_empty());
        assert_eq!(resources.blue_env_injections[0].name, "PAYMENT_SERVICE_URL");
        assert_eq!(
            resources.blue_env_injections[0].value,
            Some(format!(
                "http://{}:9090",
                inception_service_name("order-processor-bg", "payment-calls")
            ))
        );
        assert_eq!(resources.test_env_injections[0].name, "ORDER_WRITE_URL");
        assert_eq!(
            resources.test_env_injections[0].value,
            Some(format!(
                "http://{}:9090",
                inception_service_name("order-processor-bg", "payment-calls")
            ))
        );
    }

    #[test]
    fn standalone_produces_deployment_and_service() {
        let plugin = make_rabbitmq_plugin();
        let ip = make_inception_point(
            "incoming-orders",
            vec![PluginRole::Duplicator, PluginRole::Observer],
            serde_json::json!({
                "duplicator": {
                    "inputQueue": "orders",
                    "greenInputQueue": "orders-green",
                    "blueInputQueue": "orders-blue"
                },
                "observer": {
                    "testId": {"field": "queue.body", "jsonPath": "$.orderId"},
                    "match": [{"field": "queue.body", "jsonPath": "$.type", "matches": "^order$"}],
                    "notifyPath": "/trigger"
                }
            }),
        );
        let resources = reconcile_inception_point(
            &plugin,
            &ip,
            "production",
            "http://operator:8090",
            "http://test-container:8080",
            Some("/result/{testId}"),
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
            Some(inception_instance_base_name("order-processor-bg", "incoming-orders").as_str())
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
        assert_eq!(
            deploy
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap()
                .termination_grace_period_seconds,
            Some(1)
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
            Some("/result/{testId}"),
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
        let plugin = make_http_plugin();
        let ip = make_inception_point(
            "bad-point",
            vec![PluginRole::Duplicator],
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
                Some("/result/{testId}"),
                "blue",
                "order-processor-bg"
            )
            .is_err()
        );
    }

    #[test]
    fn consumer_role_supported() {
        let ip = make_inception_point(
            "consumer",
            vec![PluginRole::Consumer],
            serde_json::json!({"target": "q"}),
        );
        let consumer_plugin = InceptionPlugin::new(
            "consumer",
            InceptionPluginSpec {
                description: "Consumer".to_string(),
                image: "test:v1".to_string(),
                supported_roles: vec![PluginRole::Consumer],
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
                &consumer_plugin,
                &ip,
                "ns",
                "http://op:8090",
                "http://tc:8080",
                Some("/result/{testId}"),
                "blue",
                "order-processor-bg"
            )
            .is_ok()
        );
    }
}
