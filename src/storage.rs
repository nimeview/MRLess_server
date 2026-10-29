use crate::protocol::{
    ChatStorageInfo, DeliveryStatus, OutboxItem, StoredFileInfo, StoredFileOffer, StoredGroup,
    StoredMessage,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Hard ceiling only — UI manages space; silent 500-trim removed.
const MAX_HISTORY_SAFETY: usize = 50_000;
const MAX_OUTBOX: usize = 400;
pub const MAX_OUTBOX_ATTEMPTS: u32 = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredContact {
    pub name: String,
    #[serde(default)]
    pub last_addr: Option<String>,
    #[serde(default)]
    pub last_seen: i64,
}

pub struct Storage {
    data_dir: PathBuf,
    messages: VecDeque<StoredMessage>,
    /// Remembered display names / last addrs for peers (survive disconnect).
    contacts: HashMap<Uuid, StoredContact>,
    groups: HashMap<Uuid, StoredGroup>,
    /// group_id → seq at leave/dissolve time. Blocks stale GroupMeta from re-adding us.
    left_groups: HashMap<Uuid, u64>,
    outbox: Vec<OutboxItem>,
    file_offers: HashMap<Uuid, StoredFileOffer>,
}

impl Storage {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir.join("files")).context("create files dir")?;
        std::fs::create_dir_all(data_dir.join("avatars")).context("create avatars dir")?;
        let mut storage = Self {
            data_dir: data_dir.to_path_buf(),
            messages: VecDeque::new(),
            contacts: HashMap::new(),
            groups: HashMap::new(),
            left_groups: HashMap::new(),
            outbox: Vec::new(),
            file_offers: HashMap::new(),
        };
        storage.load_messages()?;
        storage.load_contacts()?;
        storage.load_groups()?;
        storage.load_left_groups()?;
        storage.load_outbox()?;
        storage.load_file_offers()?;
        // Seed contacts from message history (older installs without contacts.json).
        for msg in storage.messages.iter() {
            if !msg.from_name.is_empty() && msg.from_name != "node" {
                storage
                    .contacts
                    .entry(msg.from)
                    .or_insert_with(|| StoredContact {
                        name: msg.from_name.clone(),
                        last_addr: None,
                        last_seen: msg.ts,
                    });
            }
        }
        let _ = storage.persist_contacts();
        Ok(storage)
    }

    fn contacts_path(&self) -> PathBuf {
        self.data_dir.join("contacts.json")
    }

    fn outbox_path(&self) -> PathBuf {
        self.data_dir.join("outbox.json")
    }

    fn groups_path(&self) -> PathBuf {
        self.data_dir.join("groups.json")
    }

    fn left_groups_path(&self) -> PathBuf {
        self.data_dir.join("left_groups.json")
    }

    fn file_offers_path(&self) -> PathBuf {
        self.data_dir.join("file_offers.json")
    }

    fn load_left_groups(&mut self) -> Result<()> {
        let path = self.left_groups_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read left_groups")?;
        self.left_groups = serde_json::from_str(&raw).unwrap_or_default();
        Ok(())
    }

    fn persist_left_groups(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.left_groups)?;
        std::fs::write(self.left_groups_path(), raw).context("write left_groups")?;
        Ok(())
    }

    pub fn mark_left_group(&mut self, id: Uuid, seq: u64) -> Result<()> {
        let prev = self.left_groups.get(&id).copied().unwrap_or(0);
        if seq > prev {
            self.left_groups.insert(id, seq);
            self.persist_left_groups()?;
        }
        Ok(())
    }

    pub fn clear_left_group(&mut self, id: Uuid) -> Result<()> {
        if self.left_groups.remove(&id).is_some() {
            self.persist_left_groups()?;
        }
        Ok(())
    }

    pub fn left_group_seq(&self, id: Uuid) -> Option<u64> {
        self.left_groups.get(&id).copied()
    }

    fn load_file_offers(&mut self) -> Result<()> {
        let path = self.file_offers_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read file_offers")?;
        let list: Vec<StoredFileOffer> = serde_json::from_str(&raw).unwrap_or_default();
        self.file_offers = list.into_iter().map(|o| (o.file_id, o)).collect();
        Ok(())
    }

    fn persist_file_offers(&self) -> Result<()> {
        let mut list: Vec<&StoredFileOffer> = self.file_offers.values().collect();
        list.sort_by_key(|o| o.ts);
        let raw = serde_json::to_string_pretty(&list)?;
        std::fs::write(self.file_offers_path(), raw).context("write file_offers")?;
        Ok(())
    }

    pub fn upsert_file_offer(&mut self, offer: StoredFileOffer) -> Result<()> {
        self.file_offers.insert(offer.file_id, offer);
        while self.file_offers.len() > 2_000 {
            if let Some(oldest) = self
                .file_offers
                .values()
                .min_by_key(|o| o.ts)
                .map(|o| o.file_id)
            {
                self.file_offers.remove(&oldest);
            } else {
                break;
            }
        }
        self.persist_file_offers()
    }

    pub fn remove_file_offer(&mut self, file_id: Uuid) -> Result<bool> {
        let removed = self.file_offers.remove(&file_id).is_some();
        if removed {
            self.persist_file_offers()?;
        }
        Ok(removed)
    }

    pub fn file_offers(&self) -> Vec<StoredFileOffer> {
        let mut list: Vec<_> = self.file_offers.values().cloned().collect();
        list.sort_by_key(|o| o.ts);
        list
    }

    fn load_groups(&mut self) -> Result<()> {
        let path = self.groups_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read groups")?;
        self.groups = serde_json::from_str(&raw).unwrap_or_default();
        Ok(())
    }

    fn persist_groups(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.groups)?;
        std::fs::write(self.groups_path(), raw).context("write groups")?;
        Ok(())
    }

    pub fn groups(&self) -> Vec<StoredGroup> {
        let mut list: Vec<_> = self.groups.values().cloned().collect();
        list.sort_by_key(|a| a.name.to_lowercase());
        list
    }

    pub fn group(&self, id: Uuid) -> Option<StoredGroup> {
        self.groups.get(&id).cloned()
    }

    pub fn upsert_group(&mut self, mut group: StoredGroup) -> Result<bool> {
        group.members.sort();
        group.members.dedup();
        if let Some(old) = self.groups.get(&group.id) {
            if group.seq <= old.seq {
                return Ok(false);
            }
        }
        self.groups.insert(group.id, group);
        self.persist_groups()?;
        Ok(true)
    }

    pub fn is_group_member(&self, group_id: Uuid, node: Uuid) -> bool {
        self.groups
            .get(&group_id)
            .map(|g| g.members.contains(&node))
            .unwrap_or(false)
    }

    pub fn remove_group(&mut self, id: Uuid) -> Result<bool> {
        if self.groups.remove(&id).is_some() {
            self.persist_groups()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Drop group locally and record a leave tombstone so stale mesh meta cannot revive it.
    pub fn leave_group_local(&mut self, id: Uuid, seq: u64) -> Result<bool> {
        self.mark_left_group(id, seq)?;
        self.remove_group(id)
    }

    fn load_contacts(&mut self) -> Result<()> {
        let path = self.contacts_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read contacts")?;
        // New shape: map uuid -> StoredContact. Old shape: map uuid -> String.
        if let Ok(map) = serde_json::from_str::<HashMap<Uuid, StoredContact>>(&raw) {
            self.contacts = map;
            return Ok(());
        }
        if let Ok(legacy) = serde_json::from_str::<HashMap<Uuid, String>>(&raw) {
            self.contacts = legacy
                .into_iter()
                .map(|(id, name)| {
                    (
                        id,
                        StoredContact {
                            name,
                            last_addr: None,
                            last_seen: 0,
                        },
                    )
                })
                .collect();
        }
        Ok(())
    }

    fn persist_contacts(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.contacts)?;
        std::fs::write(self.contacts_path(), raw).context("write contacts")?;
        Ok(())
    }

    fn load_outbox(&mut self) -> Result<()> {
        let path = self.outbox_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read outbox")?;
        self.outbox = serde_json::from_str(&raw).unwrap_or_default();
        Ok(())
    }

    fn persist_outbox(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.outbox)?;
        std::fs::write(self.outbox_path(), raw).context("write outbox")?;
        Ok(())
    }

    pub fn remember_contact(&mut self, id: Uuid, name: &str) -> Result<bool> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(false);
        }
        let changed = match self.contacts.get_mut(&id) {
            Some(c) => {
                if c.name != name {
                    c.name = name.to_string();
                    true
                } else {
                    false
                }
            }
            None => {
                self.contacts.insert(
                    id,
                    StoredContact {
                        name: name.to_string(),
                        last_addr: None,
                        last_seen: 0,
                    },
                );
                true
            }
        };
        if changed {
            self.persist_contacts()?;
        }
        Ok(changed)
    }

    pub fn touch_contact_addr(&mut self, id: Uuid, addr: &str, ts: i64) -> Result<()> {
        let entry = self.contacts.entry(id).or_insert_with(|| StoredContact {
            name: id.to_string()[..8].to_string(),
            last_addr: None,
            last_seen: 0,
        });
        entry.last_addr = Some(addr.to_string());
        entry.last_seen = ts;
        self.persist_contacts()
    }

    pub fn contact_name(&self, id: &Uuid) -> Option<String> {
        self.contacts.get(id).map(|c| c.name.clone())
    }

    pub fn contacts(&self) -> Vec<(Uuid, StoredContact)> {
        let mut list: Vec<_> = self.contacts.iter().map(|(k, v)| (*k, v.clone())).collect();
        list.sort_by_key(|a| a.1.name.to_lowercase());
        list
    }

    pub fn known_peer_addrs(&self) -> Vec<String> {
        self.contacts
            .values()
            .filter_map(|c| c.last_addr.clone())
            .collect()
    }

    fn messages_path(&self) -> PathBuf {
        self.data_dir.join("messages.json")
    }

    fn load_messages(&mut self) -> Result<()> {
        let path = self.messages_path();
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path).context("read messages")?;
        let list: Vec<StoredMessage> = serde_json::from_str(&raw).unwrap_or_default();
        self.messages = list.into();
        Ok(())
    }

    fn persist_messages(&self) -> Result<()> {
        let list: Vec<&StoredMessage> = self.messages.iter().collect();
        let raw = serde_json::to_string_pretty(&list)?;
        std::fs::write(self.messages_path(), raw).context("write messages")?;
        Ok(())
    }

    pub fn push_message(&mut self, msg: StoredMessage) -> Result<bool> {
        if self.messages.iter().any(|m| m.id == msg.id) {
            return Ok(false);
        }
        self.messages.push_back(msg);
        while self.messages.len() > MAX_HISTORY_SAFETY {
            self.messages.pop_front();
        }
        self.persist_messages()?;
        Ok(true)
    }

    pub fn delete_message(&mut self, id: Uuid) -> Result<bool> {
        let before = self.messages.len();
        self.messages.retain(|m| m.id != id);
        if self.messages.len() != before {
            self.persist_messages()?;
            let _ = self.remove_outbox(id);
            return Ok(true);
        }
        Ok(false)
    }

    /// `chat_key`: `lan` | `g:<uuid>` | peer uuid.
    pub fn delete_messages_for_chat(&mut self, chat_key: &str, our_id: Uuid) -> Result<usize> {
        let before = self.messages.len();
        if chat_key == "lan" {
            self.messages
                .retain(|m| m.group_id.is_some() || m.to.is_some());
        } else if let Some(rest) = chat_key.strip_prefix("g:") {
            let gid: Uuid = rest.parse().context("bad group id")?;
            self.messages.retain(|m| m.group_id != Some(gid));
        } else {
            let peer: Uuid = chat_key.parse().context("bad peer id")?;
            self.messages.retain(|m| {
                if m.group_id.is_some() || m.to.is_none() {
                    return true;
                }
                let other = if m.from == our_id {
                    m.to
                } else if m.to == Some(our_id) {
                    Some(m.from)
                } else {
                    None
                };
                other != Some(peer)
            });
        }
        let removed = before - self.messages.len();
        if removed > 0 {
            self.persist_messages()?;
        }
        Ok(removed)
    }

    pub fn file_size_on_disk(path: &Path) -> u64 {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    pub fn list_cached_files(&self) -> Vec<StoredFileInfo> {
        let mut list = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.files_dir()) else {
            return list;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let Some((id_str, rest)) = name.split_once('_') else {
                continue;
            };
            let Ok(file_id) = Uuid::parse_str(id_str) else {
                continue;
            };
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let ts = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            list.push(StoredFileInfo {
                file_id,
                file_name: rest.to_string(),
                size,
                path: path.display().to_string(),
                ts,
            });
        }
        list.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.size.cmp(&a.size)));
        list
    }

    pub fn delete_file(&self, file_id: Uuid) -> Result<bool> {
        let Some(path) = self.find_file(file_id) else {
            return Ok(false);
        };
        std::fs::remove_file(&path).context("remove file")?;
        Ok(true)
    }

    pub fn purge_file_cache(&self) -> Result<usize> {
        let mut n = 0usize;
        let dir = self.files_dir();
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
            return Ok(0);
        }
        for entry in std::fs::read_dir(&dir)?.flatten() {
            if entry.path().is_file() {
                let _ = std::fs::remove_file(entry.path());
                n += 1;
            }
        }
        Ok(n)
    }

    fn path_len(path: &Path) -> u64 {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    pub fn chat_storage_summaries(&self, our_id: Uuid) -> Vec<ChatStorageInfo> {
        let mut lan_count = 0usize;
        let mut lan_bytes = 0u64;
        let mut groups: HashMap<Uuid, (usize, u64)> = HashMap::new();
        let mut dms: HashMap<Uuid, (usize, u64)> = HashMap::new();

        for m in self.messages.iter() {
            let approx = (m.body.len() + 64) as u64;
            if let Some(gid) = m.group_id {
                let e = groups.entry(gid).or_default();
                e.0 += 1;
                e.1 += approx;
            } else if m.to.is_none() {
                lan_count += 1;
                lan_bytes += approx;
            } else {
                let other = if m.from == our_id {
                    m.to.unwrap_or(m.from)
                } else {
                    m.from
                };
                let e = dms.entry(other).or_default();
                e.0 += 1;
                e.1 += approx;
            }
        }

        let mut chats = Vec::new();
        if lan_count > 0 {
            chats.push(ChatStorageInfo {
                key: "lan".into(),
                title: "LAN Mesh".into(),
                kind: "lan".into(),
                message_count: lan_count,
                approx_bytes: lan_bytes,
            });
        }
        for (gid, (count, bytes)) in groups {
            let title = self
                .groups
                .get(&gid)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| format!("group {}", &gid.to_string()[..8]));
            chats.push(ChatStorageInfo {
                key: format!("g:{gid}"),
                title,
                kind: "group".into(),
                message_count: count,
                approx_bytes: bytes,
            });
        }
        for (pid, (count, bytes)) in dms {
            let title = self
                .contact_name(&pid)
                .unwrap_or_else(|| pid.to_string()[..8].to_string());
            chats.push(ChatStorageInfo {
                key: pid.to_string(),
                title,
                kind: "dm".into(),
                message_count: count,
                approx_bytes: bytes,
            });
        }
        chats.sort_by_key(|b| std::cmp::Reverse(b.approx_bytes));
        chats
    }

    pub fn usage_bytes(&self) -> (u64, u64, u64) {
        let messages_bytes = Self::path_len(&self.messages_path());
        let files_bytes: u64 = self.list_cached_files().iter().map(|f| f.size).sum();
        let other = Self::path_len(&self.contacts_path())
            + Self::path_len(&self.groups_path())
            + Self::path_len(&self.outbox_path())
            + Self::path_len(&self.data_dir.join("config.json"))
            + Self::path_len(&self.data_dir.join("identity.json"));
        (messages_bytes, files_bytes, other)
    }

    pub fn set_message_status(&mut self, id: Uuid, status: DeliveryStatus) -> Result<bool> {
        let mut found = false;
        for m in self.messages.iter_mut() {
            if m.id == id {
                m.status = status;
                found = true;
                break;
            }
        }
        if found {
            self.persist_messages()?;
        }
        Ok(found)
    }

    pub fn history(&self) -> Vec<StoredMessage> {
        self.messages.iter().cloned().collect()
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn message(&self, id: Uuid) -> Option<StoredMessage> {
        self.messages.iter().find(|m| m.id == id).cloned()
    }

    /// Update body in place. Keeps the original timestamp.
    pub fn update_message_body(&mut self, id: Uuid, body: String) -> Result<Option<StoredMessage>> {
        let Some(msg) = self.messages.iter_mut().find(|m| m.id == id) else {
            return Ok(None);
        };
        msg.body = body;
        msg.edited = true;
        let out = msg.clone();
        self.persist_messages()?;
        Ok(Some(out))
    }

    fn avatar_path(&self, node_id: Uuid) -> PathBuf {
        self.data_dir.join("avatars").join(format!("{node_id}.bin"))
    }

    fn avatar_meta_path(&self, node_id: Uuid) -> PathBuf {
        self.data_dir
            .join("avatars")
            .join(format!("{node_id}.mime"))
    }

    /// Max decoded avatar bytes (UI compresses before upload).
    pub const MAX_AVATAR_BYTES: usize = 48 * 1024;

    pub fn set_avatar(&self, node_id: Uuid, mime: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > Self::MAX_AVATAR_BYTES {
            return Err(anyhow::anyhow!("avatar too large"));
        }
        std::fs::create_dir_all(self.data_dir.join("avatars")).context("create avatars dir")?;
        let mime = sanitize_avatar_mime(mime);
        std::fs::write(self.avatar_path(node_id), bytes).context("write avatar")?;
        std::fs::write(self.avatar_meta_path(node_id), mime).context("write avatar mime")?;
        Ok(())
    }

    pub fn clear_avatar(&self, node_id: Uuid) -> Result<()> {
        let _ = std::fs::remove_file(self.avatar_path(node_id));
        let _ = std::fs::remove_file(self.avatar_meta_path(node_id));
        Ok(())
    }

    pub fn get_avatar(&self, node_id: Uuid) -> Option<(String, Vec<u8>)> {
        let bytes = std::fs::read(self.avatar_path(node_id)).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let mime = std::fs::read_to_string(self.avatar_meta_path(node_id))
            .unwrap_or_else(|_| "image/jpeg".into());
        Some((sanitize_avatar_mime(&mime), bytes))
    }

    pub fn list_avatar_ids(&self) -> Vec<Uuid> {
        let dir = self.data_dir.join("avatars");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".bin") {
                if let Ok(id) = stem.parse::<Uuid>() {
                    out.push(id);
                }
            }
        }
        out
    }

    pub fn outbox_item(&self, id: Uuid) -> Option<OutboxItem> {
        self.outbox.iter().find(|i| i.id == id).cloned()
    }

    pub fn enqueue_outbox(&mut self, item: OutboxItem) -> Result<()> {
        if self.outbox.iter().any(|x| x.id == item.id) {
            return Ok(());
        }
        self.outbox.push(item);
        while self.outbox.len() > MAX_OUTBOX {
            self.outbox.remove(0);
        }
        self.persist_outbox()
    }

    pub fn due_outbox(&self, now: i64) -> Vec<OutboxItem> {
        self.outbox
            .iter()
            .filter(|i| i.next_retry <= now)
            .cloned()
            .collect()
    }

    pub fn mark_outbox_attempt(&mut self, id: Uuid, now: i64) -> Result<Option<OutboxItem>> {
        let Some(item) = self.outbox.iter_mut().find(|i| i.id == id) else {
            return Ok(None);
        };
        item.attempts += 1;
        let backoff = (2u64.pow(item.attempts.min(6))).max(2);
        item.next_retry = now + backoff as i64;
        let clone = item.clone();
        self.persist_outbox()?;
        Ok(Some(clone))
    }

    pub fn remove_outbox(&mut self, id: Uuid) -> Result<bool> {
        let before = self.outbox.len();
        self.outbox.retain(|i| i.id != id);
        if self.outbox.len() != before {
            self.persist_outbox()?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn outbox_ids(&self) -> Vec<Uuid> {
        self.outbox.iter().map(|i| i.id).collect()
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }

    pub fn store_file(&self, file_id: Uuid, file_name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let safe: String = Path::new(file_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.bin")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(80)
            .collect();
        let path = self.files_dir().join(format!("{file_id}_{safe}"));
        std::fs::write(&path, bytes).context("write file")?;
        Ok(path)
    }

    pub fn wipe(&mut self) -> Result<()> {
        self.messages.clear();
        self.contacts.clear();
        self.groups.clear();
        self.left_groups.clear();
        self.outbox.clear();
        self.file_offers.clear();
        let _ = std::fs::remove_file(self.messages_path());
        let _ = std::fs::remove_file(self.contacts_path());
        let _ = std::fs::remove_file(self.groups_path());
        let _ = std::fs::remove_file(self.left_groups_path());
        let _ = std::fs::remove_file(self.outbox_path());
        let _ = std::fs::remove_file(self.file_offers_path());
        let dir = self.files_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).ok();
        }
        std::fs::create_dir_all(self.files_dir()).context("recreate files dir")?;
        let avatars = self.data_dir.join("avatars");
        if avatars.exists() {
            std::fs::remove_dir_all(&avatars).ok();
        }
        std::fs::create_dir_all(&avatars).ok();
        self.persist_messages()?;
        self.persist_contacts()?;
        self.persist_groups()?;
        self.persist_outbox()?;
        self.persist_file_offers()?;
        Ok(())
    }

    pub fn find_file(&self, file_id: Uuid) -> Option<PathBuf> {
        let prefix = format!("{file_id}_");
        std::fs::read_dir(self.files_dir()).ok()?.find_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) {
                Some(entry.path())
            } else {
                None
            }
        })
    }
}

fn sanitize_avatar_mime(mime: &str) -> String {
    let m = mime.trim().to_ascii_lowercase();
    match m.as_str() {
        "image/png" | "image/webp" | "image/gif" => m,
        _ => "image/jpeg".into(),
    }
}
