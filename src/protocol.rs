use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 3;

/// Wire packets between mesh peers (newline-delimited JSON over TCP).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeshPacket {
    Hello {
        node_id: Uuid,
        name: String,
        tcp_port: u16,
        version: u32,
        #[serde(default)]
        token: Option<String>,
    },
    Ping {
        from: Uuid,
        ts: i64,
    },
    Pong {
        from: Uuid,
        ts: i64,
    },
    Presence {
        node_id: Uuid,
        name: String,
        seq: u64,
        ts: i64,
        #[serde(default)]
        addr: Option<String>,
        /// Short 8-char invite code advertised with this node.
        #[serde(default)]
        code: Option<String>,
        ttl: u8,
    },
    Chat {
        id: Uuid,
        from: Uuid,
        from_name: String,
        /// `None` = broadcast to the LAN room (when `group_id` is also none).
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
        body: String,
        ttl: u8,
        ts: i64,
        #[serde(default)]
        reply_to: Option<Uuid>,
        /// Snapshot for UI when the parent message is missing locally.
        #[serde(default)]
        reply_author: Option<String>,
        #[serde(default)]
        reply_snippet: Option<String>,
        /// When true, peers update an existing message with this `id` instead of inserting.
        #[serde(default)]
        edited: bool,
    },
    GroupMeta {
        id: Uuid,
        name: String,
        members: Vec<Uuid>,
        owner: Uuid,
        seq: u64,
        ts: i64,
        ttl: u8,
    },
    Ack {
        id: Uuid,
        from: Uuid,
        to: Uuid,
        status: DeliveryStatus,
        ttl: u8,
    },
    FileOffer {
        id: Uuid,
        from: Uuid,
        from_name: String,
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
        file_id: Uuid,
        file_name: String,
        size: u64,
        sha256: String,
        ttl: u8,
        ts: i64,
    },
    FileRequest {
        id: Uuid,
        file_id: Uuid,
        from: Uuid,
        to: Uuid,
        ttl: u8,
    },
    FileChunk {
        id: Uuid,
        file_id: Uuid,
        from: Uuid,
        to: Uuid,
        offset: u64,
        data_b64: String,
        done: bool,
        ttl: u8,
    },
    /// Profile photo flood. Empty `data_b64` clears the avatar.
    Avatar {
        node_id: Uuid,
        mime: String,
        data_b64: String,
        ts: i64,
        ttl: u8,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Queued,
    Sent,
    #[default]
    Delivered,
    Failed,
}

/// Messages between the local UI and this node (WebSocket JSON).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiIn {
    SetName {
        name: String,
    },
    Send {
        body: String,
        #[serde(default)]
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
        #[serde(default)]
        reply_to: Option<Uuid>,
    },
    SendFile {
        file_name: String,
        data_b64: String,
        #[serde(default)]
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
    },
    CreateGroup {
        name: String,
        members: Vec<Uuid>,
    },
    UpdateGroup {
        id: Uuid,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        members: Option<Vec<Uuid>>,
    },
    LeaveGroup {
        id: Uuid,
    },
    /// Owner dissolves the group for everyone on the mesh.
    DeleteGroup {
        id: Uuid,
    },
    RequestFile {
        file_id: Uuid,
        from: Uuid,
    },
    Resend {
        id: Uuid,
    },
    GetConfig,
    SetConfig {
        patch: crate::config::ConfigPatch,
    },
    CompleteOnboarding {
        name: String,
    },
    AddManualPeer {
        addr: String,
    },
    RemoveManualPeer {
        addr: String,
    },
    ReconnectPeers,
    ResetNode,
    GetMeshStats,
    GetInvite,
    /// Re-detect listen address and mint a fresh invite code when needed.
    RefreshInvite,
    GetStorage,
    /// Delete all messages for a chat. Keys: `lan`, `g:<uuid>`, or peer uuid.
    DeleteMessages {
        chat_key: String,
    },
    DeleteFile {
        file_id: Uuid,
    },
    /// Remove all files under the local cache dir; keep message text.
    PurgeFileCache,
    DeleteMessage {
        id: Uuid,
    },
    /// Load a cached file as base64 so the UI can Save outside the app.
    ExportFile {
        file_id: Uuid,
    },
    EditMessage {
        id: Uuid,
        body: String,
    },
    /// Re-send an existing message to another chat.
    Forward {
        id: Uuid,
        #[serde(default)]
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
    },
    /// Drop a file offer from the local chat timeline (cache bytes optional).
    DeleteFileOffer {
        file_id: Uuid,
    },
    /// JPEG/PNG/WebP base64, max ~48 KiB decoded. Empty clears.
    SetAvatar {
        data_b64: String,
        #[serde(default)]
        mime: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiOut {
    Hello {
        node_id: Uuid,
        name: String,
        mesh_port: u16,
        ws_port: u16,
        onboarded: bool,
    },
    NeedOnboarding {
        node_id: Uuid,
    },
    Peers {
        peers: Vec<PeerInfo>,
    },
    /// Remembered peer display names (including currently offline peers).
    Contacts {
        contacts: Vec<ContactInfo>,
    },
    Groups {
        groups: Vec<GroupInfo>,
    },
    Message {
        id: Uuid,
        from: Uuid,
        from_name: String,
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
        body: String,
        ts: i64,
        mine: bool,
        #[serde(default)]
        status: DeliveryStatus,
        #[serde(default)]
        reply_to: Option<Uuid>,
        #[serde(default)]
        reply_author: Option<String>,
        #[serde(default)]
        reply_snippet: Option<String>,
        #[serde(default)]
        edited: bool,
    },
    MessageStatus {
        id: Uuid,
        status: DeliveryStatus,
    },
    FileAvailable {
        id: Uuid,
        from: Uuid,
        from_name: String,
        to: Option<Uuid>,
        #[serde(default)]
        group_id: Option<Uuid>,
        file_id: Uuid,
        file_name: String,
        size: u64,
        sha256: String,
        ts: i64,
        local: bool,
    },
    FileReceived {
        file_id: Uuid,
        file_name: String,
        path: String,
        size: u64,
    },
    Config {
        config: AppConfig,
        restart_required: bool,
        display_name: String,
        node_id: Uuid,
        onboarded: bool,
    },
    MeshStats {
        peer_count: usize,
        uptime_secs: u64,
        onboarded: bool,
        discovery_enabled: bool,
        #[serde(default)]
        mesh_reachable: bool,
        #[serde(default)]
        invite: Option<String>,
        #[serde(default)]
        invite_code: Option<String>,
        #[serde(default)]
        listen_addr: Option<String>,
    },
    Invite {
        url: String,
        #[serde(default)]
        code: String,
        listen: String,
        node_id: Uuid,
    },
    Status {
        text: String,
    },
    Error {
        text: String,
    },
    Cleared,
    StorageUsage {
        total_bytes: u64,
        messages_bytes: u64,
        files_bytes: u64,
        other_bytes: u64,
        message_count: usize,
        files: Vec<StoredFileInfo>,
        chats: Vec<ChatStorageInfo>,
    },
    /// Messages for this chat key were deleted locally.
    MessagesDeleted {
        chat_key: String,
    },
    FileDeleted {
        file_id: Uuid,
    },
    FileCachePurged,
    MessageDeleted {
        id: Uuid,
    },
    FileExport {
        file_id: Uuid,
        file_name: String,
        data_b64: String,
        size: u64,
    },
    FileOfferDeleted {
        file_id: Uuid,
    },
    /// Profile photo for a node. Empty `data_b64` means no avatar.
    Avatar {
        node_id: Uuid,
        mime: String,
        data_b64: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFileInfo {
    pub file_id: Uuid,
    pub file_name: String,
    pub size: u64,
    pub path: String,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStorageInfo {
    /// `lan`, `g:<uuid>`, or peer uuid string.
    pub key: String,
    pub title: String,
    pub kind: String,
    pub message_count: usize,
    pub approx_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: Uuid,
    pub name: String,
    pub addr: String,
    #[serde(default)]
    pub last_seen: i64,
    #[serde(default)]
    pub source: String,
    pub rtt_ms: Option<u64>,
    #[serde(default)]
    pub reach: ReachKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReachKind {
    #[default]
    Direct,
    Mesh,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfo {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<Uuid>,
    pub owner: Uuid,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub last_addr: Option<String>,
    #[serde(default)]
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Uuid,
    pub from: Uuid,
    pub from_name: String,
    pub to: Option<Uuid>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    pub body: String,
    pub ts: i64,
    #[serde(default)]
    pub status: DeliveryStatus,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
    #[serde(default)]
    pub reply_author: Option<String>,
    #[serde(default)]
    pub reply_snippet: Option<String>,
    #[serde(default)]
    pub edited: bool,
}

/// Persisted file offer so bubbles survive restart (bytes may still be missing until Get).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFileOffer {
    pub id: Uuid,
    pub from: Uuid,
    pub from_name: String,
    pub to: Option<Uuid>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    pub file_id: Uuid,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroup {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<Uuid>,
    pub owner: Uuid,
    pub seq: u64,
    pub updated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: Uuid,
    pub to: Option<Uuid>,
    pub packet: MeshPacket,
    pub attempts: u32,
    pub next_retry: i64,
    pub created: i64,
}

pub fn now_ts() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
