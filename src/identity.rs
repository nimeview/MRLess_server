use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::protocol::now_ts;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub node_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub onboarded: bool,
    #[serde(default)]
    pub created_at: i64,
}

impl Identity {
    pub fn load_or_create(data_dir: &Path, display_name: Option<String>) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("create data dir")?;
        let path = data_dir.join("identity.json");
        if path.exists() {
            let raw = std::fs::read_to_string(&path).context("read identity")?;
            let mut id: Identity = serde_json::from_str(&raw).context("parse identity")?;
            if id.created_at == 0 {
                id.created_at = now_ts();
            }
            if let Some(name) = display_name {
                let name = sanitize_name(&name);
                let mut dirty = false;
                if id.name != name {
                    id.name = name;
                    dirty = true;
                }
                if !id.onboarded {
                    id.onboarded = true;
                    dirty = true;
                }
                if dirty {
                    id.save(data_dir)?;
                }
            }
            return Ok(id);
        }

        let explicit = display_name.is_some();
        let id = Identity {
            node_id: Uuid::new_v4(),
            name: sanitize_name(&display_name.unwrap_or_else(|| "node".into())),
            onboarded: explicit,
            created_at: now_ts(),
        };
        id.save(data_dir)?;
        Ok(id)
    }

    pub fn complete_onboarding(&mut self, data_dir: &Path, name: &str) -> Result<()> {
        self.name = sanitize_name(name);
        self.onboarded = true;
        if self.created_at == 0 {
            self.created_at = now_ts();
        }
        self.save(data_dir)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join("identity.json");
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw).context("write identity")?;
        Ok(())
    }
}

pub fn sanitize_name(name: &str) -> String {
    let trimmed = name.trim();
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    if cleaned.is_empty() {
        format!("peer-{}", &Uuid::new_v4().to_string()[..8])
    } else {
        cleaned
    }
}

pub fn default_data_dir(instance: &str) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mrless")
        .join(instance)
}
