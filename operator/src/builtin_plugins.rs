use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{DeleteParams, Patch, PatchParams};
use kube::{Api, ResourceExt};
use serde::Deserialize;
use tokio::time::sleep;
use tracing::info;

use crate::crd::inception_plugin::InceptionPlugin;

const INCEPTION_PLUGIN_CRD: &str = "inceptionplugins.fluidbg.io";

pub async fn run(mode: &str, manifest_path: &str) -> Result<()> {
    let client = kube::Client::try_default()
        .await
        .context("failed to create kubernetes client")?;
    wait_for_inception_plugin_crd(&client).await?;

    let plugins = read_plugins(manifest_path)?;
    match mode {
        "apply" => apply_plugins(client, plugins).await,
        "delete" => delete_plugins(client, plugins).await,
        other => bail!("unsupported builtin plugin hook mode: {other}"),
    }
}

fn read_plugins(path: &str) -> Result<Vec<InceptionPlugin>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read builtin plugin manifests from {path}"))?;
    let mut plugins = Vec::new();

    for document in serde_yaml::Deserializer::from_str(&contents) {
        let value = serde_yaml::Value::deserialize(document)
            .context("failed to deserialize builtin plugin manifest document")?;
        if matches!(value, serde_yaml::Value::Null) {
            continue;
        }
        let plugin: InceptionPlugin =
            serde_yaml::from_value(value).context("failed to parse InceptionPlugin manifest")?;
        plugins.push(plugin);
    }

    Ok(plugins)
}

async fn apply_plugins(client: kube::Client, plugins: Vec<InceptionPlugin>) -> Result<()> {
    let params = PatchParams::apply("fluidbg-builtin-plugin-hook").force();
    for plugin in plugins {
        let namespace = plugin
            .namespace()
            .context("builtin InceptionPlugin manifest is missing metadata.namespace")?;
        let name = plugin.name_any();
        let api: Api<InceptionPlugin> = Api::namespaced(client.clone(), &namespace);
        api.patch(&name, &params, &Patch::Apply(&plugin))
            .await
            .with_context(|| {
                format!("failed to apply builtin InceptionPlugin {namespace}/{name}")
            })?;
        info!("applied builtin InceptionPlugin {namespace}/{name}");
    }
    Ok(())
}

async fn delete_plugins(client: kube::Client, plugins: Vec<InceptionPlugin>) -> Result<()> {
    for plugin in plugins {
        let namespace = plugin
            .namespace()
            .context("builtin InceptionPlugin manifest is missing metadata.namespace")?;
        let name = plugin.name_any();
        let api: Api<InceptionPlugin> = Api::namespaced(client.clone(), &namespace);
        match api.delete(&name, &DeleteParams::default()).await {
            Ok(_) => info!("deleted builtin InceptionPlugin {namespace}/{name}"),
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to delete builtin InceptionPlugin {namespace}/{name}")
                });
            }
        }
    }
    Ok(())
}

async fn wait_for_inception_plugin_crd(client: &kube::Client) -> Result<()> {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());

    for _ in 0..120 {
        match api.get(INCEPTION_PLUGIN_CRD).await {
            Ok(crd) if crd_is_established(&crd) => return Ok(()),
            Ok(_) | Err(kube::Error::Api(_)) => sleep(Duration::from_secs(1)).await,
            Err(error) => return Err(error).context("failed to read InceptionPlugin CRD"),
        }
    }

    bail!("timed out waiting for {INCEPTION_PLUGIN_CRD} to become established")
}

fn crd_is_established(crd: &CustomResourceDefinition) -> bool {
    crd.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Established" && condition.status == "True")
        })
}

#[cfg(test)]
mod tests {
    use super::read_plugins;

    #[test]
    fn reads_multi_document_plugin_manifests() {
        let path = std::env::temp_dir().join(format!(
            "fluidbg-builtin-plugins-{}.yaml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"---
apiVersion: fluidbg.io/v1alpha1
kind: InceptionPlugin
metadata:
  name: http
  namespace: fluidbg-test
spec:
  description: http
  image: http:dev
  supportedRoles: [observer]
  topology: standalone
  fieldNamespaces: [http]
  configSchema: {type: object}
  inceptor: {}
---
apiVersion: fluidbg.io/v1alpha1
kind: InceptionPlugin
metadata:
  name: rabbitmq
  namespace: fluidbg-test
spec:
  description: rabbitmq
  image: rabbitmq:dev
  supportedRoles: [consumer]
  topology: standalone
  fieldNamespaces: [queue]
  configSchema: {type: object}
  inceptor: {}
"#,
        )
        .expect("write manifest");

        let plugins = read_plugins(path.to_str().expect("utf-8 path")).expect("plugins");

        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].metadata.name.as_deref(), Some("http"));
        assert_eq!(plugins[1].metadata.name.as_deref(), Some("rabbitmq"));
        std::fs::remove_file(path).ok();
    }
}
