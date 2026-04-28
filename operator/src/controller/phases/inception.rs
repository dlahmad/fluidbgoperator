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
    test_instance_name, test_service_port,
};
use crate::crd::blue_green::{BlueGreenDeployment, InceptionPoint};
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
    let test_container_url = if let Some(test) = bgd.spec.tests.first() {
        let port = test_service_port(test)?;
        format!(
            "http://{}.{}:{}",
            test_instance_name(bgd, &test.name),
            namespace,
            port
        )
    } else {
        "http://localhost:8080".to_string()
    };
    let test_data_verify_path = bgd
        .spec
        .tests
        .first()
        .and_then(|test| test.data_verification.as_ref())
        .map(|verification| verification.verify_path.as_str());
    let mut plans = Vec::new();
    let mut pre_prepare_test_assignments = Vec::new();
    let mut post_prepare_assignments = Vec::new();

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
        let (test_assignments, other_assignments) = split_test_assignments(template_assignments(
            resources.green_env_injections,
            resources.blue_env_injections,
            resources.test_env_injections,
        ));
        pre_prepare_test_assignments.extend(test_assignments);
        post_prepare_assignments.extend(other_assignments);

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

        plans.push(PreparedInceptionPoint {
            inception_point: ip.clone(),
            plugin,
            plugin_deployments,
        });
    }

    if !pre_prepare_test_assignments.is_empty() {
        let touched =
            apply_assignments(bgd, client, namespace, &pre_prepare_test_assignments, false).await?;
        wait_for_deployments_ready(client, &touched).await?;
    }

    for plan in &plans {
        if !plan.plugin_deployments.is_empty() {
            wait_for_deployments_ready(client, &plan.plugin_deployments).await?;
        }

        invoke_plugin_manager_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            &plan.inception_point,
            &plan.plugin,
            auth,
            PluginLifecycleStage::Prepare,
        )
        .await?;

        if let Some(mut lifecycle_assignments) = invoke_inceptor_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            plan.inception_point.name.as_str(),
            &plan.plugin,
            auth,
            PluginLifecycleStage::Prepare,
        )
        .await?
        {
            reject_lifecycle_test_assignments(
                &plan.inception_point.name,
                &lifecycle_assignments.assignments,
            )?;
            post_prepare_assignments.append(&mut lifecycle_assignments.assignments);
        }
    }

    if !post_prepare_assignments.is_empty() {
        let touched =
            apply_assignments(bgd, client, namespace, &post_prepare_assignments, false).await?;
        wait_for_deployments_ready(client, &touched).await?;
    }

    Ok(())
}

struct PreparedInceptionPoint {
    inception_point: InceptionPoint,
    plugin: InceptionPlugin,
    plugin_deployments: Vec<DeploymentIdentity>,
}

fn split_test_assignments(
    assignments: Vec<PropertyAssignment>,
) -> (Vec<PropertyAssignment>, Vec<PropertyAssignment>) {
    assignments
        .into_iter()
        .partition(|assignment| matches!(assignment.target, AssignmentTarget::Test))
}

fn reject_lifecycle_test_assignments(
    inception_point: &str,
    assignments: &[PropertyAssignment],
) -> std::result::Result<(), ReconcileError> {
    if assignments
        .iter()
        .any(|assignment| matches!(assignment.target, AssignmentTarget::Test))
    {
        Err(ReconcileError::Store(format!(
            "inception point '{inception_point}' returned test deployment assignments from prepare; test assignments must be declared in InceptionPlugin injects so the operator can apply them before inceptor activation"
        )))
    } else {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(target: AssignmentTarget, name: &str) -> PropertyAssignment {
        PropertyAssignment {
            target,
            kind: AssignmentKind::Env,
            name: name.to_string(),
            value: "value".to_string(),
            container_name: None,
        }
    }

    #[test]
    fn split_test_assignments_keeps_test_assignments_pre_prepare() {
        let (test, other) = split_test_assignments(vec![
            assignment(AssignmentTarget::Green, "GREEN_ENV"),
            assignment(AssignmentTarget::Test, "TEST_ENV"),
            assignment(AssignmentTarget::Blue, "BLUE_ENV"),
        ]);

        assert_eq!(test.len(), 1);
        assert_eq!(test[0].name, "TEST_ENV");
        assert_eq!(other.len(), 2);
    }

    #[test]
    fn lifecycle_prepare_may_not_return_test_assignments() {
        let err = reject_lifecycle_test_assignments(
            "observer",
            &[assignment(AssignmentTarget::Test, "TEST_ENV")],
        )
        .expect_err("test assignments from prepare must be rejected");

        assert!(format!("{err}").contains("observer"));
    }
}
