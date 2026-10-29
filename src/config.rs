use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub mesh_port: u16,
    pub ws_port: u16,
    pub discovery_port: u16,
    pub bind_mesh: String,
    pub bind_ws: String,
    pub discovery_enabled: bool,
    pub discovery_interval_ms: u64,
    pub ttl_default: u8,
    pub max_body_bytes: usize,
    pub max_file_bytes: usize,
    pub heartbeat_interval_ms: u64,
    pub peer_timeout_ms: u64,
    pub manual_peers: Vec<String>,
    pub relay_enabled: bool,
    pub accept_broadcast: bool,
    pub max_peers: usize,
    pub max_seen_ids: usize,
    pub language: String,
    pub log_level: String,
    /// Optional shared secret sent in Hello. Empty = open mesh.
    #[serde(default)]
    pub mesh_token: String,
    /// Fallback 8-char invite when the listen addr cannot be encoded.
    #[serde(default)]
    pub invite_code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigPatch {
    pub mesh_port: Option<u16>,
    pub ws_port: Option<u16>,
    pub discovery_port: Option<u16>,
    pub bind_mesh: Option<String>,
    pub bind_ws: Option<String>,
    pub discovery_enabled: Option<bool>,
    pub discovery_interval_ms: Option<u64>,
    pub ttl_default: Option<u8>,
    pub max_body_bytes: Option<usize>,
    pub max_file_bytes: Option<usize>,
    pub heartbeat_interval_ms: Option<u64>,
    pub peer_timeout_ms: Option<u64>,
    pub manual_peers: Option<Vec<String>>,
    pub relay_enabled: Option<bool>,
    pub accept_broadcast: Option<bool>,
    pub max_peers: Option<usize>,
    pub max_seen_ids: Option<usize>,
    pub language: Option<String>,
    pub log_level: Option<String>,
    pub mesh_token: Option<String>,
    pub invite_code: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    pub mesh_port: Option<u16>,
    pub ws_port: Option<u16>,
    pub discovery_port: Option<u16>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            mesh_port: 7421,
            ws_port: 7422,
            discovery_port: 7420,
            bind_mesh: "0.0.0.0".into(),
            bind_ws: "127.0.0.1".into(),
            discovery_enabled: true,
            discovery_interval_ms: 2000,
            ttl_default: 6,
            max_body_bytes: 32 * 1024,
            max_file_bytes: 8 * 1024 * 1024,
            heartbeat_interval_ms: 4000,
            peer_timeout_ms: 15000,
            manual_peers: Vec::new(),
            relay_enabled: true,
            accept_broadcast: true,
            max_peers: 64,
            max_seen_ids: 10_000,
            language: "en".into(),
            log_level: "info".into(),
            mesh_token: String::new(),
            invite_code: String::new(),
        }
    }
}

impl AppConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }

    pub fn load_or_create(data_dir: &Path, cli: &CliOverrides) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("create data dir")?;
        let path = Self::path(data_dir);
        let mut cfg = if path.exists() {
            let raw = std::fs::read_to_string(&path).context("read config")?;
            serde_json::from_str::<AppConfig>(&raw).with_context(|| {
                format!(
                    "parse config {} (fix JSON or delete the file to recreate defaults)",
                    path.display()
                )
            })?
        } else {
            AppConfig::default()
        };

        if let Some(p) = cli.mesh_port {
            cfg.mesh_port = p;
        }
        if let Some(p) = cli.ws_port {
            cfg.ws_port = p;
        }
        if let Some(p) = cli.discovery_port {
            cfg.discovery_port = p;
        }
        cfg.validate()?;
        cfg.save(data_dir)?;
        Ok(cfg)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.validate()?;
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(Self::path(data_dir), raw).context("write config")?;
        Ok(())
    }

    pub fn apply_patch(&mut self, patch: ConfigPatch) -> Result<bool> {
        let before = (
            self.mesh_port,
            self.ws_port,
            self.discovery_port,
            self.bind_mesh.clone(),
            self.bind_ws.clone(),
        );

        if let Some(v) = patch.mesh_port {
            self.mesh_port = v;
        }
        if let Some(v) = patch.ws_port {
            self.ws_port = v;
        }
        if let Some(v) = patch.discovery_port {
            self.discovery_port = v;
        }
        if let Some(v) = patch.bind_mesh {
            self.bind_mesh = v;
        }
        if let Some(v) = patch.bind_ws {
            self.bind_ws = v;
        }
        if let Some(v) = patch.discovery_enabled {
            self.discovery_enabled = v;
        }
        if let Some(v) = patch.discovery_interval_ms {
            self.discovery_interval_ms = v.max(250);
        }
        if let Some(v) = patch.ttl_default {
            self.ttl_default = v.clamp(1, 32);
        }
        if let Some(v) = patch.max_body_bytes {
            self.max_body_bytes = v.clamp(256, 1024 * 1024);
        }
        if let Some(v) = patch.max_file_bytes {
            self.max_file_bytes = v.clamp(1024, 32 * 1024 * 1024);
        }
        if let Some(v) = patch.heartbeat_interval_ms {
            self.heartbeat_interval_ms = v.max(500);
        }
        if let Some(v) = patch.peer_timeout_ms {
            self.peer_timeout_ms = v.max(1000);
        }
        if let Some(v) = patch.manual_peers {
            self.manual_peers = normalize_peers(v)?;
        }
        if let Some(v) = patch.relay_enabled {
            self.relay_enabled = v;
        }
        if let Some(v) = patch.accept_broadcast {
            self.accept_broadcast = v;
        }
        if let Some(v) = patch.max_peers {
            self.max_peers = v.clamp(1, 512);
        }
        if let Some(v) = patch.max_seen_ids {
            self.max_seen_ids = v.clamp(64, 100_000);
        }
        if let Some(v) = patch.language {
            self.language = v.chars().take(8).collect();
        }
        if let Some(v) = patch.log_level {
            self.log_level = sanitize_log_level(&v);
        }
        if let Some(v) = patch.mesh_token {
            self.mesh_token = v.chars().take(128).collect();
        }
        if let Some(v) = patch.invite_code {
            self.invite_code = v
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect::<String>()
                .to_ascii_uppercase();
        }
        self.validate()?;
        Ok(before
            != (
                self.mesh_port,
                self.ws_port,
                self.discovery_port,
                self.bind_mesh.clone(),
                self.bind_ws.clone(),
            ))
    }

    pub fn validate(&self) -> Result<()> {
        if self.mesh_port == 0 || self.ws_port == 0 || self.discovery_port == 0 {
            return Err(anyhow!("ports must be non-zero"));
        }
        if self.bind_mesh.parse::<std::net::IpAddr>().is_err() {
            return Err(anyhow!("invalid bind_mesh"));
        }
        if self.bind_ws.parse::<std::net::IpAddr>().is_err() {
            return Err(anyhow!("invalid bind_ws"));
        }
        normalize_peers(self.manual_peers.clone())?;
        Ok(())
    }

    pub fn parse_manual_addrs(&self) -> Vec<SocketAddr> {
        self.manual_peers
            .iter()
            .filter_map(|s| s.parse::<SocketAddr>().ok())
            .collect()
    }
}

pub fn normalize_peers(list: Vec<String>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for raw in list {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        s.parse::<SocketAddr>()
            .map_err(|_| anyhow!("invalid peer addr: {s}"))?;
        if !out.iter().any(|x: &String| x == s) {
            out.push(s.to_string());
        }
    }
    Ok(out)
}

fn sanitize_log_level(v: &str) -> String {
    match v.trim().to_ascii_lowercase().as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => v.trim().to_ascii_lowercase(),
        _ => "info".into(),
    }
}
