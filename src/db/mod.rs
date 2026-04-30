use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::types::{AppDb, Combo, ModelAliasTarget, ProviderConnection, ProviderNode, UsageDb};

#[derive(Debug, Clone, Default)]
pub struct ProviderConnectionFilter {
    pub provider: Option<String>,
    pub is_active: Option<bool>,
}

pub struct Db {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub usage_path: PathBuf,
    pub snapshot: ArcSwap<AppDb>,
    pub usage_snapshot: ArcSwap<UsageDb>,
    write_lock: RwLock<()>,
}

impl Db {
    pub async fn load() -> anyhow::Result<Self> {
        let data_dir = std::env::var_os("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_data_dir);

        Self::load_from(&data_dir).await
    }

    pub async fn load_from(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir).await?;

        let db_path = data_dir.join("db.json");
        let usage_path = data_dir.join("usage.json");

        let app_db = load_or_init_app_db(&db_path).await?;
        let usage_db = load_or_init_usage_db(&usage_path).await?;

        Ok(Self {
            data_dir,
            db_path,
            usage_path,
            snapshot: ArcSwap::from_pointee(app_db),
            usage_snapshot: ArcSwap::from_pointee(usage_db),
            write_lock: RwLock::new(()),
        })
    }

    pub fn snapshot(&self) -> Arc<AppDb> {
        self.snapshot.load_full()
    }

    pub fn usage_snapshot(&self) -> Arc<UsageDb> {
        self.usage_snapshot.load_full()
    }

    pub fn provider_connections(
        &self,
        filter: ProviderConnectionFilter,
    ) -> Vec<ProviderConnection> {
        let snapshot = self.snapshot();
        let mut connections: Vec<_> = snapshot
            .provider_connections
            .iter()
            .filter(|connection| {
                let provider_matches = filter
                    .provider
                    .as_ref()
                    .is_none_or(|provider| &connection.provider == provider);
                let activity_matches = filter
                    .is_active
                    .is_none_or(|is_active| connection.is_active() == is_active);

                provider_matches && activity_matches
            })
            .cloned()
            .collect();

        connections.sort_by_key(|connection| connection.priority.unwrap_or(999));
        connections
    }

    pub fn provider_nodes(&self, node_type: Option<&str>) -> Vec<ProviderNode> {
        let snapshot = self.snapshot();
        snapshot
            .provider_nodes
            .iter()
            .filter(|node| node_type.is_none_or(|expected| node.r#type == expected))
            .cloned()
            .collect()
    }

    pub fn combo_by_name(&self, name: &str) -> Option<Combo> {
        let snapshot = self.snapshot();
        snapshot
            .combos
            .iter()
            .find(|combo| combo.name == name)
            .cloned()
    }

    pub fn model_aliases(&self) -> Arc<std::collections::BTreeMap<String, ModelAliasTarget>> {
        let snapshot = self.snapshot();
        Arc::new(snapshot.model_aliases.clone())
    }

    pub async fn update<F>(&self, updater: F) -> anyhow::Result<Arc<AppDb>>
    where
        F: FnOnce(&mut AppDb),
    {
        let _guard = self.write_lock.write().await;
        let mut next = (*self.snapshot()).clone();
        updater(&mut next);
        next.normalize();
        write_json_atomic(&self.db_path, &next).await?;
        let next = Arc::new(next);
        self.snapshot.store(next.clone());
        Ok(next)
    }

    pub async fn update_usage<F>(&self, updater: F) -> anyhow::Result<Arc<UsageDb>>
    where
        F: FnOnce(&mut UsageDb),
    {
        let _guard = self.write_lock.write().await;
        let mut next = (*self.usage_snapshot()).clone();
        updater(&mut next);
        next.normalize();
        write_json_atomic(&self.usage_path, &next).await?;
        let next = Arc::new(next);
        self.usage_snapshot.store(next.clone());
        Ok(next)
    }
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let preferred = home.join(".9router");
    let legacy = home.join(".openproxy");

    if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    }
}

async fn load_or_init_app_db(path: &Path) -> anyhow::Result<AppDb> {
    if !fs::try_exists(path).await? {
        let value = AppDb::default();
        write_json_atomic(path, &value).await?;
        return Ok(value);
    }

    let bytes = fs::read(path).await?;
    let parsed = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => {
            let value = AppDb::default();
            write_json_atomic(path, &value).await?;
            return Ok(value);
        }
    };

    let value = AppDb::from_json_value(parsed.clone());
    if serde_json::to_value(&value)? != parsed {
        write_json_atomic(path, &value).await?;
    }
    Ok(value)
}

async fn load_or_init_usage_db(path: &Path) -> anyhow::Result<UsageDb> {
    if !fs::try_exists(path).await? {
        let value = UsageDb::default();
        write_json_atomic(path, &value).await?;
        return Ok(value);
    }

    let bytes = fs::read(path).await?;
    let parsed = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => {
            let value = UsageDb::default();
            write_json_atomic(path, &value).await?;
            return Ok(value);
        }
    };

    let value = UsageDb::from_json_value(parsed.clone());
    if serde_json::to_value(&value)? != parsed {
        write_json_atomic(path, &value).await?;
    }
    Ok(value)
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).await?;

    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("db"),
        Uuid::new_v4()
    ));

    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&temp_path, bytes).await?;
    fs::rename(&temp_path, path).await?;
    Ok(())
}
