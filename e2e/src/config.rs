use std::env;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateStore {
    Memory,
    Postgres,
}

impl StateStore {
    pub fn from_env() -> Result<Self> {
        match env::var("E2E_STATE_STORE")
            .unwrap_or_else(|_| "memory".to_string())
            .as_str()
        {
            "memory" => Ok(Self::Memory),
            "postgres" => Ok(Self::Postgres),
            other => bail!("unsupported E2E_STATE_STORE={other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Postgres => "postgres",
        }
    }
}

#[derive(Debug, Clone)]
pub struct E2eConfig {
    pub namespace: String,
    pub system_namespace: String,
    pub kind_cluster: Option<String>,
    pub build_images: bool,
    pub state_store: StateStore,
    pub operator_replicas: u32,
    pub root_dir: PathBuf,
    pub deploy_dir: PathBuf,
}

impl E2eConfig {
    pub fn from_env() -> Result<Self> {
        let root_dir = command::repo_root()?;
        Ok(Self {
            namespace: env::var("NS").unwrap_or_else(|_| "fluidbg-test".to_string()),
            system_namespace: env::var("NS_SYSTEM")
                .unwrap_or_else(|_| "fluidbg-system".to_string()),
            kind_cluster: env::var("KIND_CLUSTER")
                .ok()
                .filter(|value| !value.is_empty()),
            build_images: env::var("BUILD_IMAGES").unwrap_or_else(|_| "1".to_string()) == "1",
            state_store: StateStore::from_env()?,
            operator_replicas: env::var("OPERATOR_REPLICAS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
            deploy_dir: root_dir.join("e2e/deploy"),
            root_dir,
        })
    }

    pub fn deploy_file(&self, file: &str) -> String {
        self.deploy_dir.join(file).to_string_lossy().to_string()
    }
}
