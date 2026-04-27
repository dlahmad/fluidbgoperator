use k8s_openapi::api::core::v1::EnvVar;
use kube::api::Api;

use super::super::AuthConfig;
use super::super::ReconcileError;
use super::super::deployments::{
    DeploymentIdentity, apply_assignments, candidate_ref, wait_for_deployments_ready,
};
use super::super::plugin_lifecycle::{
    AssignmentKind, AssignmentTarget, PluginLifecycleStage, PropertyAssignment,
    invoke_inceptor_lifecycle, invoke_plugin_manager_lifecycle,
};
use super::super::resources::{
    apply_resource, ensure_inception_point_owned_resources, sign_inception_auth_token,
    test_instance_name,
};
use crate::crd::blue_green::BlueGreenDeployment;
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::{ReconcileInceptionContext, reconcile_inception_point};

pub(in crate::controller) async fn ensure_inception_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    let operator_url = "http://fluidbg-operator.fluidbg-system:8090";
    let test_container_url = bgd
        .spec
        .tests
        .first()
        .map(|test| {
            format!(
                "http://{}.{}:{}",
                test_instance_name(bgd, &test.name),
                namespace,
                test.port
            )
        })
        .unwrap_or_else(|| "http://localhost:8080".to_string());
    let test_data_verify_path = bgd
        .spec
        .tests
        .first()
        .and_then(|test| test.data_verification.as_ref())
        .map(|verification| verification.verify_path.as_str());
    let mut all_assignments = Vec::new();

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        let auth_token = sign_inception_auth_token(
            client,
            namespace,
            auth,
            bgd.metadata.name.as_deref().unwrap_or(""),
            &ip.name,
            &plugin,
        )
        .await?;
        ensure_inception_point_owned_resources(client, namespace, ip).await?;
        let resources = reconcile_inception_point(
            &plugin,
            ip,
            ReconcileInceptionContext {
                namespace,
                operator_url,
                test_container_url: &test_container_url,
                test_data_verify_path,
                blue_deployment_name: &candidate_ref(bgd).name,
                blue_green_ref: bgd.metadata.name.as_deref().unwrap_or(""),
                auth_token: &auth_token,
            },
        )
        .map_err(ReconcileError::Store)?;
        let mut plugin_deployments = Vec::new();
        all_assignments.extend(template_assignments(
            resources.green_env_injections,
            resources.blue_env_injections,
            resources.test_env_injections,
        ));

        for cm in resources.config_maps {
            let name =
                cm.metadata.name.clone().ok_or_else(|| {
                    ReconcileError::Store("generated ConfigMap has no name".into())
                })?;
            apply_resource(Api::namespaced(client.clone(), namespace), &name, &cm).await?;
        }
        for deployment in resources.deployments {
            let name =
                deployment.metadata.name.clone().ok_or_else(|| {
                    ReconcileError::Store("generated Deployment has no name".into())
                })?;
            let deployment_namespace = deployment
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| namespace.to_string());
            apply_resource(
                Api::namespaced(client.clone(), &deployment_namespace),
                &name,
                &deployment,
            )
            .await?;
            plugin_deployments.push(DeploymentIdentity {
                namespace: deployment_namespace,
                name,
            });
        }
        for service in resources.services {
            let name = service
                .metadata
                .name
                .clone()
                .ok_or_else(|| ReconcileError::Store("generated Service has no name".into()))?;
            apply_resource(Api::namespaced(client.clone(), namespace), &name, &service).await?;
        }

        if !plugin_deployments.is_empty() {
            wait_for_deployments_ready(client, &plugin_deployments).await?;
        }

        invoke_plugin_manager_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip,
            &plugin,
            auth,
            PluginLifecycleStage::Prepare,
        )
        .await?;

        if let Some(mut lifecycle_assignments) = invoke_inceptor_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            auth,
            PluginLifecycleStage::Prepare,
        )
        .await?
        {
            all_assignments.append(&mut lifecycle_assignments.assignments);
        }
    }

    if !all_assignments.is_empty() {
        let touched = apply_assignments(bgd, client, namespace, &all_assignments, false).await?;
        wait_for_deployments_ready(client, &touched).await?;
    }

    Ok(())
}

fn template_assignments(
    green_env_injections: Vec<EnvVar>,
    blue_env_injections: Vec<EnvVar>,
    test_env_injections: Vec<EnvVar>,
) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    assignments.extend(
        green_env_injections
            .into_iter()
            .map(|env| PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }),
    );
    assignments.extend(
        blue_env_injections
            .into_iter()
            .map(|env| PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }),
    );
    assignments.extend(
        test_env_injections
            .into_iter()
            .map(|env| PropertyAssignment {
                target: AssignmentTarget::Test,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }),
    );
    assignments
}
