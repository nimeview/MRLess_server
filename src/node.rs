use crate::config::{AppConfig, ConfigPatch};
use crate::identity::{sanitize_name, Identity};
use crate::invite_code::{self, looks_like_code, normalize_code};
use crate::protocol::{
    now_ts, ContactInfo, DeliveryStatus, GroupInfo, MeshPacket, OutboxItem, PeerInfo, ReachKind,
    StoredGroup, StoredMessage, UiIn, UiOut, PROTOCOL_VERSION,
};
use crate::storage::{Storage, MAX_OUTBOX_ATTEMPTS};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, watch, Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

const DISCOVERY_MAGIC: &[u8] = b"MRLESS1";
const MDNS_SERVICE: &str = "_mrless._tcp.local.";
const MAX_MESH_LINE: usize = 12 * 1024 * 1024;
const PEER_QUEUE: usize = 256;
const MAX_CACHED_OFFERS: usize = 2_000;
const AVATAR_TTL: u8 = 6;

struct SeenSet {
    set: HashSet<Uuid>,
    order: VecDeque<Uuid>,
    cap: usize,
}

impl SeenSet {
    fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(64),
        }
    }

    fn insert(&mut self, id: Uuid) -> bool {
        if !self.set.insert(id) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(64);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
    }
}

struct PeerLink {
    name: String,
    addr: SocketAddr,
    tx: tokio::sync::mpsc::Sender<String>,
    last_rx: Instant,
    source: &'static str,
    rtt_ms: Option<u64>,
    ping_sent: Option<Instant>,
    cancel: watch::Sender<bool>,
}

#[derive(Clone)]
struct CachedOffer {
    id: Uuid,
    from: Uuid,
    from_name: String,
    to: Option<Uuid>,
    group_id: Option<Uuid>,
    file_id: Uuid,
    file_name: String,
    size: u64,
    sha256: String,
    ts: i64,
}

struct Inner {
    identity: Identity,
    storage: Storage,
    config: AppConfig,
    peers: HashMap<Uuid, PeerLink>,
    seen: SeenSet,
    /// Currently in-flight TCP connect attempts (dedup).
    known_addrs: HashSet<SocketAddr>,
    /// Last-known mesh addresses we should keep trying (discovered + manual).
    remembered_addrs: HashMap<SocketAddr, &'static str>,
    offers: HashMap<Uuid, CachedOffer>,
    presence: HashMap<Uuid, PresenceHint>,
    /// Short invite code → last advertised mesh addr (discovery / presence).
    code_index: HashMap<String, SocketAddr>,
    presence_seq: u64,
    restart_required: bool,
    last_peers_sig: Option<String>,
    last_contacts_sig: Option<String>,
    last_stats_sig: Option<String>,
}

#[derive(Clone)]
struct PresenceHint {
    name: String,
    seq: u64,
    ts: i64,
    addr: Option<String>,
    code: Option<String>,
}

async fn peer_send(peer: &PeerLink, line: String) {
    if let Err(e) = peer.tx.send(line).await {
        tracing::debug!("peer {} queue: {e}", peer.addr);
    }
}

#[derive(Clone)]
pub struct Node {
    data_dir: PathBuf,
    bind_mesh: (IpAddr, u16),
    bind_ws: (IpAddr, u16),
    discovery_port: u16,
    started_at: Instant,
    inner: Arc<Mutex<Inner>>,
    ui_tx: broadcast::Sender<UiOut>,
    our_tcp: Arc<RwLock<Option<SocketAddr>>>,
}

impl Node {
    pub fn start(data_dir: PathBuf, cfg: AppConfig, identity: Identity) -> Result<Self> {
        let storage = Storage::open(&data_dir)?;
        let mut offers = HashMap::new();
        for o in storage.file_offers() {
            offers.insert(
                o.file_id,
                CachedOffer {
                    id: o.id,
                    from: o.from,
                    from_name: o.from_name,
                    to: o.to,
                    group_id: o.group_id,
                    file_id: o.file_id,
                    file_name: o.file_name,
                    size: o.size,
                    sha256: o.sha256,
                    ts: o.ts,
                },
            );
        }
        let bind_mesh = (
            cfg.bind_mesh
                .parse::<IpAddr>()
                .unwrap_or(IpAddr::from([0, 0, 0, 0])),
            cfg.mesh_port,
        );
        let bind_ws = (
            cfg.bind_ws
                .parse::<IpAddr>()
                .unwrap_or(IpAddr::from([127, 0, 0, 1])),
            cfg.ws_port,
        );
        let discovery_port = cfg.discovery_port;
        let seen_cap = cfg.max_seen_ids;
        let (ui_tx, _) = broadcast::channel(256);
        let node = Node {
            data_dir,
            bind_mesh,
            bind_ws,
            discovery_port,
            started_at: Instant::now(),
            inner: Arc::new(Mutex::new(Inner {
                identity,
                storage,
                config: cfg,
                peers: HashMap::new(),
                seen: SeenSet::new(seen_cap),
                known_addrs: HashSet::new(),
                remembered_addrs: HashMap::new(),
                offers,
                presence: HashMap::new(),
                code_index: HashMap::new(),
                presence_seq: 0,
                restart_required: false,
                last_peers_sig: None,
                last_contacts_sig: None,
                last_stats_sig: None,
            })),
            ui_tx,
            our_tcp: Arc::new(RwLock::new(None)),
        };

        let n = node.clone();
        tokio::spawn(async move {
            if let Err(e) = n.run_mesh_listener().await {
                tracing::error!("mesh listener failed: {e:#}");
            }
        });
        let n = node.clone();
        tokio::spawn(async move {
            if let Err(e) = n.run_discovery().await {
                tracing::error!("discovery failed: {e:#}");
            }
        });
        let n = node.clone();
        tokio::spawn(async move {
            if let Err(e) = n.run_ws().await {
                tracing::error!("websocket failed: {e:#}");
            }
        });
        let n = node.clone();
        tokio::spawn(async move { n.run_heartbeat().await });
        let n = node.clone();
        tokio::spawn(async move { n.run_peer_redial().await });
        let n = node.clone();
        tokio::spawn(async move { n.run_outbox().await });
        let n = node.clone();
        tokio::spawn(async move { n.run_presence().await });
        let n = node.clone();
        tokio::spawn(async move { n.run_stats().await });
        let n = node.clone();
        tokio::spawn(async move {
            if let Err(e) = n.run_mdns().await {
                tracing::warn!("mDNS discovery: {e:#}");
            }
        });

        Ok(node)
    }

    async fn identity_snapshot(&self) -> (Uuid, String, bool) {
        let g = self.inner.lock().await;
        (
            g.identity.node_id,
            g.identity.name.clone(),
            g.identity.onboarded,
        )
    }

    async fn announce_status(&self, text: impl Into<String>) {
        let _ = self.ui_tx.send(UiOut::Status { text: text.into() });
    }

    fn collect_peers_locked(g: &Inner) -> Vec<PeerInfo> {
        let mut list: Vec<PeerInfo> = g
            .peers
            .iter()
            .map(|(id, p)| PeerInfo {
                id: *id,
                name: p.name.clone(),
                addr: p.addr.to_string(),
                last_seen: now_ts() - p.last_rx.elapsed().as_secs() as i64,
                source: p.source.to_string(),
                rtt_ms: p.rtt_ms,
                reach: ReachKind::Direct,
            })
            .collect();
        let timeout = (g.config.peer_timeout_ms / 1000).max(5) as i64 + 8;
        let now = now_ts();
        for (id, hint) in g.presence.iter() {
            if g.peers.contains_key(id) {
                continue;
            }
            if now - hint.ts > timeout * 4 {
                continue;
            }
            list.push(PeerInfo {
                id: *id,
                name: hint.name.clone(),
                addr: hint.addr.clone().unwrap_or_default(),
                last_seen: hint.ts,
                source: "mesh".into(),
                rtt_ms: None,
                reach: ReachKind::Mesh,
            });
        }
        list.sort_by_key(|a| a.id);
        list
    }

    fn peers_sig(peers: &[PeerInfo]) -> String {
        peers
            .iter()
            .map(|p| format!("{}|{}|{}|{:?}|{}", p.id, p.name, p.addr, p.reach, p.source))
            .collect::<Vec<_>>()
            .join(";")
    }

    fn contacts_sig(contacts: &[ContactInfo]) -> String {
        contacts
            .iter()
            .map(|c| {
                format!(
                    "{}|{}|{}",
                    c.id,
                    c.name,
                    c.last_addr.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    async fn snapshot_peers(&self) -> Vec<PeerInfo> {
        let g = self.inner.lock().await;
        Self::collect_peers_locked(&g)
    }

    async fn snapshot_contacts(&self) -> Vec<ContactInfo> {
        let g = self.inner.lock().await;
        g.storage
            .contacts()
            .into_iter()
            .map(|(id, c)| ContactInfo {
                id,
                name: c.name,
                last_addr: c.last_addr,
                last_seen: c.last_seen,
            })
            .collect()
    }

    async fn snapshot_groups(&self) -> Vec<GroupInfo> {
        let g = self.inner.lock().await;
        g.storage.groups().into_iter().map(group_to_info).collect()
    }

    async fn publish_peers(&self) {
        let peers = self.snapshot_peers().await;
        let sig = Self::peers_sig(&peers);
        {
            let mut g = self.inner.lock().await;
            if g.last_peers_sig.as_deref() == Some(sig.as_str()) {
                return;
            }
            g.last_peers_sig = Some(sig);
        }
        let _ = self.ui_tx.send(UiOut::Peers { peers });
    }

    async fn publish_contacts(&self) {
        let contacts = self.snapshot_contacts().await;
        let sig = Self::contacts_sig(&contacts);
        {
            let mut g = self.inner.lock().await;
            if g.last_contacts_sig.as_deref() == Some(sig.as_str()) {
                return;
            }
            g.last_contacts_sig = Some(sig);
        }
        let _ = self.ui_tx.send(UiOut::Contacts { contacts });
    }

    async fn publish_groups(&self) {
        let groups = self.snapshot_groups().await;
        let _ = self.ui_tx.send(UiOut::Groups { groups });
    }

    async fn remember_contact(&self, id: Uuid, name: &str) {
        let changed = {
            let mut g = self.inner.lock().await;
            match g.storage.remember_contact(id, name) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("contact save: {e:#}");
                    false
                }
            }
        };
        if changed {
            self.publish_contacts().await;
        }
    }

    async fn publish_config(&self) {
        let out = {
            let g = self.inner.lock().await;
            UiOut::Config {
                config: g.config.clone(),
                restart_required: g.restart_required,
                display_name: g.identity.name.clone(),
                node_id: g.identity.node_id,
                onboarded: g.identity.onboarded,
            }
        };
        let _ = self.ui_tx.send(out);
    }

    async fn publish_hello(&self) {
        let out = {
            let g = self.inner.lock().await;
            UiOut::Hello {
                node_id: g.identity.node_id,
                name: g.identity.name.clone(),
                mesh_port: self.bind_mesh.1,
                ws_port: self.bind_ws.1,
                onboarded: g.identity.onboarded,
            }
        };
        let _ = self.ui_tx.send(out);
        let onboarded = self.inner.lock().await.identity.onboarded;
        if !onboarded {
            let node_id = self.inner.lock().await.identity.node_id;
            let _ = self.ui_tx.send(UiOut::NeedOnboarding { node_id });
        }
    }

    async fn publish_stats(&self) {
        self.ensure_invite_code_persisted().await;
        let out = {
            let mut g = self.inner.lock().await;
            let peer_count = g.peers.len();
            let onboarded = g.identity.onboarded;
            let discovery_enabled = g.config.discovery_enabled;
            let mesh_reachable = !g.peers.is_empty();
            let invite = self.invite_url(&g);
            let invite_code = self.invite_code_for(&g);
            let listen_addr = advertise_addr(self.bind_mesh.0, self.bind_mesh.1);
            let sig = format!(
                "{peer_count}|{onboarded}|{discovery_enabled}|{mesh_reachable}|{invite_code}|{listen_addr}"
            );
            if g.last_stats_sig.as_deref() == Some(sig.as_str()) {
                return;
            }
            g.last_stats_sig = Some(sig);
            UiOut::MeshStats {
                peer_count,
                uptime_secs: self.started_at.elapsed().as_secs(),
                onboarded,
                discovery_enabled,
                mesh_reachable,
                invite: Some(invite),
                invite_code: Some(invite_code),
                listen_addr: Some(listen_addr),
            }
        };
        let _ = self.ui_tx.send(out);
    }

    async fn run_stats(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            self.publish_stats().await;
        }
    }

    async fn run_heartbeat(&self) {
        loop {
            let (interval, timeout) = {
                let g = self.inner.lock().await;
                (
                    Duration::from_millis(g.config.heartbeat_interval_ms),
                    Duration::from_millis(g.config.peer_timeout_ms),
                )
            };
            tokio::time::sleep(interval).await;
            let from = self.inner.lock().await.identity.node_id;
            let ping = MeshPacket::Ping { from, ts: now_ts() };
            let line = match serde_json::to_string(&ping) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut drop_ids = Vec::new();
            let mut ping_peers = Vec::new();
            {
                let mut g = self.inner.lock().await;
                let now = Instant::now();
                for (id, peer) in g.peers.iter_mut() {
                    if now.duration_since(peer.last_rx) > timeout {
                        drop_ids.push(*id);
                        continue;
                    }
                    peer.ping_sent = Some(now);
                    ping_peers.push(peer.tx.clone());
                }
            }
            for tx in ping_peers {
                let _ = tx.send(line.clone()).await;
            }
            for id in drop_ids {
                self.drop_peer(id, "timeout").await;
            }
        }
    }

    async fn drop_peer(&self, id: Uuid, reason: &str) {
        let mut g = self.inner.lock().await;
        if let Some(peer) = g.peers.remove(&id) {
            tracing::info!("dropping peer {} ({reason})", short_id(id));
            let _ = peer.cancel.send(true);
            g.known_addrs.remove(&peer.addr);
            g.remembered_addrs.insert(peer.addr, peer.source);
        }
        drop(g);
        self.publish_peers().await;
    }

    async fn run_peer_redial(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            let mut jobs: Vec<(SocketAddr, &'static str)> = Vec::new();
            {
                let mut g = self.inner.lock().await;
                for addr in g.config.parse_manual_addrs() {
                    g.remembered_addrs.entry(addr).or_insert("manual");
                }
                for addr in g.storage.known_peer_addrs() {
                    if let Ok(parsed) = addr.parse::<SocketAddr>() {
                        g.remembered_addrs.entry(parsed).or_insert("contact");
                    }
                }
                for (addr, source) in g.remembered_addrs.iter() {
                    if g.peers.values().any(|p| &p.addr == addr) {
                        continue;
                    }
                    jobs.push((*addr, *source));
                }
            }
            for (addr, source) in jobs {
                self.connect_peer(addr, source).await;
            }
        }
    }

    async fn run_outbox(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            self.flush_outbox().await;
        }
    }

    async fn flood_presence(&self) {
        self.ensure_invite_code_persisted().await;
        let pkt = {
            let mut g = self.inner.lock().await;
            if g.peers.is_empty() {
                None
            } else {
                g.presence_seq = g.presence_seq.saturating_add(1);
                let code = self.invite_code_for(&g);
                Some(MeshPacket::Presence {
                    node_id: g.identity.node_id,
                    name: g.identity.name.clone(),
                    seq: g.presence_seq,
                    ts: now_ts(),
                    addr: Some(advertise_addr(self.bind_mesh.0, self.bind_mesh.1)),
                    code: Some(code),
                    ttl: g.config.ttl_default,
                })
            }
        };
        if let Some(pkt) = pkt {
            self.relay(&pkt, None).await;
        }
    }

    async fn run_presence(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(4));
        loop {
            tick.tick().await;
            self.flood_presence().await;
        }
    }

    async fn run_mesh_listener(&self) -> Result<()> {
        let addr = SocketAddr::new(self.bind_mesh.0, self.bind_mesh.1);
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind mesh TCP {addr}"))?;
        let local = listener.local_addr()?;
        *self.our_tcp.write().await = Some(local);
        tracing::info!("mesh TCP listening on {local}");
        self.announce_status(format!("mesh listening on {local}"))
            .await;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let node = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node
                    .handle_peer_stream(stream, peer_addr, true, "discovered")
                    .await
                {
                    tracing::debug!("peer {peer_addr} closed: {e:#}");
                }
            });
        }
    }

    /// Outbound connect used by UDP/mDNS discovery and native (iOS) Bonjour.
    pub async fn connect_peer(&self, addr: SocketAddr, source: &'static str) {
        if let Some(local) = *self.our_tcp.read().await {
            if addrs_same_endpoint(local, addr) {
                return;
            }
        }
        {
            let mut g = self.inner.lock().await;
            g.remembered_addrs.entry(addr).or_insert(source);
            if g.known_addrs.contains(&addr) {
                return;
            }
            if g.peers.values().any(|p| p.addr == addr) {
                return;
            }
            g.known_addrs.insert(addr);
        }

        match TcpStream::connect(addr).await {
            Ok(stream) => {
                let node = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = node.handle_peer_stream(stream, addr, false, source).await {
                        tracing::debug!("outbound peer {addr} closed: {e:#}");
                    }
                    let mut g = node.inner.lock().await;
                    g.known_addrs.remove(&addr);
                });
            }
            Err(e) => {
                tracing::warn!("connect {addr} failed: {e}");
                let mut g = self.inner.lock().await;
                g.known_addrs.remove(&addr);
            }
        }
    }

    async fn handle_peer_stream(
        &self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        inbound: bool,
        source: &'static str,
    ) -> Result<()> {
        let _ = stream.set_nodelay(true);
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let hello = {
            let g = self.inner.lock().await;
            let token = g.config.mesh_token.trim();
            MeshPacket::Hello {
                node_id: g.identity.node_id,
                name: g.identity.name.clone(),
                tcp_port: self.bind_mesh.1,
                version: PROTOCOL_VERSION,
                token: if token.is_empty() {
                    None
                } else {
                    Some(token.to_string())
                },
            }
        };
        writer
            .write_all((serde_json::to_string(&hello)? + "\n").as_bytes())
            .await?;

        let first = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("peer closed before hello"))?;
        if first.len() > MAX_MESH_LINE {
            return Err(anyhow!("hello line too large"));
        }
        let packet: MeshPacket = serde_json::from_str(&first)?;
        let (peer_id, peer_name, hello_token) = match packet {
            MeshPacket::Hello {
                node_id,
                name,
                version,
                token,
                ..
            } => {
                if version != PROTOCOL_VERSION {
                    return Err(anyhow!("protocol mismatch"));
                }
                let our_id = self.inner.lock().await.identity.node_id;
                if node_id == our_id {
                    return Err(anyhow!("self-connection"));
                }
                (node_id, name, token)
            }
            _ => return Err(anyhow!("expected hello")),
        };
        {
            let expected = self.inner.lock().await.config.mesh_token.trim().to_string();
            if !expected.is_empty() {
                match hello_token {
                    Some(t) if t == expected => {}
                    _ => return Err(anyhow!("mesh token rejected")),
                }
            }
        }

        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(PEER_QUEUE);
        let write_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        });

        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        {
            let mut g = self.inner.lock().await;
            if g.peers.contains_key(&peer_id) {
                write_task.abort();
                return Err(anyhow!("already linked to {}", short_id(peer_id)));
            }
            if g.peers.len() >= g.config.max_peers {
                write_task.abort();
                return Err(anyhow!("max_peers reached"));
            }
            g.known_addrs.insert(peer_addr);
            g.peers.insert(
                peer_id,
                PeerLink {
                    name: peer_name.clone(),
                    addr: peer_addr,
                    tx: out_tx.clone(),
                    last_rx: Instant::now(),
                    source,
                    rtt_ms: None,
                    ping_sent: None,
                    cancel: cancel_tx,
                },
            );
        }
        tracing::info!(
            "peer linked {} ({peer_name}) via {peer_addr} inbound={inbound} source={source}",
            short_id(peer_id)
        );
        self.remember_contact(peer_id, &peer_name).await;
        {
            let mut g = self.inner.lock().await;
            g.remembered_addrs.insert(peer_addr, source);
            let _ = g
                .storage
                .touch_contact_addr(peer_id, &peer_addr.to_string(), now_ts());
        }
        self.announce_status(format!("peer online: {peer_name}"))
            .await;
        self.publish_peers().await;
        self.flush_outbox().await;
        self.flood_presence().await;
        self.announce_owned_groups().await;
        self.share_our_avatar().await;

        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        break;
                    }
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if line.len() > MAX_MESH_LINE {
                                tracing::warn!("dropping oversized line from {peer_addr}");
                                break;
                            }
                            if line.trim().is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<MeshPacket>(&line) {
                                Ok(pkt) => {
                                    if let Err(e) = self.on_mesh_packet(pkt, Some(peer_id)).await {
                                        tracing::warn!("packet handling: {e:#}");
                                    }
                                }
                                Err(e) => tracing::debug!("bad packet from {peer_addr}: {e}"),
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }

        {
            let mut g = self.inner.lock().await;
            g.peers.remove(&peer_id);
            g.known_addrs.remove(&peer_addr);
        }
        write_task.abort();
        self.announce_status(format!("peer offline: {peer_name}"))
            .await;
        self.publish_peers().await;
        Ok(())
    }

    async fn touch_peer(&self, via: Option<Uuid>) {
        if let Some(id) = via {
            if let Some(p) = self.inner.lock().await.peers.get_mut(&id) {
                p.last_rx = Instant::now();
            }
        }
    }

    async fn on_mesh_packet(&self, packet: MeshPacket, via: Option<Uuid>) -> Result<()> {
        self.touch_peer(via).await;
        match packet {
            MeshPacket::Hello { node_id, name, .. } => {
                if let Some(p) = self.inner.lock().await.peers.get_mut(&node_id) {
                    p.name = name.clone();
                }
                self.remember_contact(node_id, &name).await;
                self.publish_peers().await;
                Ok(())
            }
            MeshPacket::Ping { from, ts } => {
                let our = self.inner.lock().await.identity.node_id;
                let pong = MeshPacket::Pong { from: our, ts };
                let line = serde_json::to_string(&pong)?;
                if let Some(id) = via {
                    if let Some(peer) = self.inner.lock().await.peers.get(&id) {
                        peer_send(peer, line).await;
                    }
                } else if let Some(peer) = self.inner.lock().await.peers.get(&from) {
                    peer_send(peer, line).await;
                }
                Ok(())
            }
            MeshPacket::Pong { from, .. } => {
                if let Some(p) = self.inner.lock().await.peers.get_mut(&from) {
                    if let Some(sent) = p.ping_sent.take() {
                        p.rtt_ms = Some(sent.elapsed().as_millis() as u64);
                    }
                }
                Ok(())
            }
            MeshPacket::Presence {
                node_id,
                name,
                seq,
                ts,
                addr,
                code,
                ttl,
            } => {
                self.ingest_presence(node_id, name, seq, ts, addr, code, ttl, via)
                    .await
            }
            MeshPacket::Ack {
                id,
                from,
                to,
                status,
                ttl,
            } => self.ingest_ack(id, from, to, status, ttl, via).await,
            MeshPacket::Chat {
                id,
                from,
                from_name,
                to,
                group_id,
                body,
                ttl,
                ts,
                reply_to,
                reply_author,
                reply_snippet,
                edited,
            } => {
                self.ingest_chat(
                    id,
                    from,
                    from_name,
                    to,
                    group_id,
                    body,
                    ttl,
                    ts,
                    reply_to,
                    reply_author,
                    reply_snippet,
                    edited,
                    via,
                )
                .await
            }
            MeshPacket::GroupMeta {
                id,
                name,
                members,
                owner,
                seq,
                ts,
                ttl,
            } => {
                self.ingest_group_meta(id, name, members, owner, seq, ts, ttl, via)
                    .await
            }
            MeshPacket::FileOffer {
                id,
                from,
                from_name,
                to,
                group_id,
                file_id,
                file_name,
                size,
                sha256,
                ttl,
                ts,
            } => {
                self.ingest_file_offer(
                    id, from, from_name, to, group_id, file_id, file_name, size, sha256, ttl, ts,
                    via,
                )
                .await
            }
            MeshPacket::FileRequest {
                id,
                file_id,
                from,
                to,
                ttl,
            } => {
                self.ingest_file_request(id, file_id, from, to, ttl, via)
                    .await
            }
            MeshPacket::FileChunk {
                id,
                file_id,
                from,
                to,
                offset,
                data_b64,
                done,
                ttl,
            } => {
                self.ingest_file_chunk(id, file_id, from, to, offset, data_b64, done, ttl, via)
                    .await
            }
            MeshPacket::Avatar {
                node_id,
                mime,
                data_b64,
                ts,
                ttl,
            } => {
                self.ingest_avatar(node_id, mime, data_b64, ts, ttl, via)
                    .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_chat(
        &self,
        id: Uuid,
        from: Uuid,
        from_name: String,
        to: Option<Uuid>,
        group_id: Option<Uuid>,
        body: String,
        ttl: u8,
        ts: i64,
        reply_to: Option<Uuid>,
        reply_author: Option<String>,
        reply_snippet: Option<String>,
        edited: bool,
        via: Option<Uuid>,
    ) -> Result<()> {
        if !edited {
            let mut g = self.inner.lock().await;
            if !g.seen.insert(id) {
                return Ok(());
            }
        } else {
            // Dedup identical edit floods (same id+ts+len) from the mesh.
            let dedup = Uuid::from_u128(
                id.as_u128()
                    ^ ((ts as u128) << 32)
                    ^ (body.len() as u128).wrapping_mul(0x9E37_79B9),
            );
            let mut g = self.inner.lock().await;
            if !g.seen.insert(dedup) {
                return Ok(());
            }
        }

        let (our_id, accept_broadcast, relay_enabled, member) = {
            let g = self.inner.lock().await;
            let member = match group_id {
                Some(gid) => {
                    g.storage.is_group_member(gid, g.identity.node_id) || from == g.identity.node_id
                }
                None => true,
            };
            (
                g.identity.node_id,
                g.config.accept_broadcast,
                g.config.relay_enabled,
                member,
            )
        };
        let for_us = if group_id.is_some() {
            member
        } else {
            match to {
                None => accept_broadcast || from == our_id,
                Some(dest) => dest == our_id || from == our_id,
            }
        };

        if for_us {
            let contact_changed = {
                let mut g = self.inner.lock().await;
                g.storage
                    .remember_contact(from, &from_name)
                    .unwrap_or(false)
            };

            if edited {
                let updated = {
                    let mut g = self.inner.lock().await;
                    let existing = g.storage.message(id);
                    if existing.as_ref().map(|m| m.from) != Some(from) {
                        None
                    } else {
                        // Keep original ts — only body/edited flag change.
                        g.storage.update_message_body(id, body.clone())?
                    }
                };
                if let Some(msg) = updated {
                    let _ = self.ui_tx.send(UiOut::Message {
                        id: msg.id,
                        from: msg.from,
                        from_name: msg.from_name,
                        to: msg.to,
                        group_id: msg.group_id,
                        body: msg.body,
                        ts: msg.ts,
                        mine: from == our_id,
                        status: msg.status,
                        reply_to: msg.reply_to,
                        reply_author: msg.reply_author,
                        reply_snippet: msg.reply_snippet,
                        edited: true,
                    });
                }
            } else {
                let stored = StoredMessage {
                    id,
                    from,
                    from_name: from_name.clone(),
                    to,
                    group_id,
                    body: body.clone(),
                    ts,
                    status: if from == our_id {
                        DeliveryStatus::Queued
                    } else {
                        DeliveryStatus::Delivered
                    },
                    reply_to,
                    reply_author: reply_author.clone(),
                    reply_snippet: reply_snippet.clone(),
                    edited: false,
                };
                let is_new = {
                    let mut g = self.inner.lock().await;
                    g.storage.push_message(stored)?
                };
                if is_new {
                    let status = if from == our_id {
                        DeliveryStatus::Queued
                    } else {
                        DeliveryStatus::Delivered
                    };
                    let _ = self
                        .inner
                        .lock()
                        .await
                        .storage
                        .set_message_status(id, status);
                    let _ = self.ui_tx.send(UiOut::Message {
                        id,
                        from,
                        from_name: from_name.clone(),
                        to,
                        group_id,
                        body: body.clone(),
                        ts,
                        mine: from == our_id,
                        status,
                        reply_to,
                        reply_author: reply_author.clone(),
                        reply_snippet: reply_snippet.clone(),
                        edited: false,
                    });
                }
            }
            if contact_changed {
                self.publish_contacts().await;
            }
            if !edited && from != our_id && to == Some(our_id) && group_id.is_none() {
                let ack = MeshPacket::Ack {
                    id,
                    from: our_id,
                    to: from,
                    status: DeliveryStatus::Delivered,
                    ttl: 8,
                };
                self.relay(&ack, None).await;
            }
        }

        let local_origin = via.is_none() && from == our_id;
        // Edits from us are flooded once (not via outbox). Normal local sends use outbox.
        let should_relay = if edited && local_origin {
            ttl > 1
        } else {
            relay_enabled && ttl > 1 && !local_origin
        };
        if should_relay {
            let relay = MeshPacket::Chat {
                id,
                from,
                from_name: from_name.clone(),
                to,
                group_id,
                body,
                ttl: ttl - 1,
                ts,
                reply_to,
                reply_author,
                reply_snippet,
                edited,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_presence(
        &self,
        node_id: Uuid,
        name: String,
        seq: u64,
        ts: i64,
        addr: Option<String>,
        code: Option<String>,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        if node_id == our {
            return Ok(());
        }
        let mut visual = false;
        {
            let mut g = self.inner.lock().await;
            let keep = match g.presence.get(&node_id) {
                Some(old) => seq > old.seq || (seq == old.seq && ts > old.ts),
                None => true,
            };
            if keep {
                visual = match g.presence.get(&node_id) {
                    Some(old) => old.name != name || old.addr != addr || old.code != code,
                    None => true,
                };
                g.presence.insert(
                    node_id,
                    PresenceHint {
                        name: name.clone(),
                        seq,
                        ts,
                        addr: addr.clone(),
                        code: code.clone(),
                    },
                );
                let _ = g.storage.remember_contact(node_id, &name);
                if let Some(ref a) = addr {
                    let _ = g.storage.touch_contact_addr(node_id, a, ts);
                    if let Ok(parsed) = a.parse::<SocketAddr>() {
                        g.remembered_addrs.entry(parsed).or_insert("presence");
                        if let Some(ref c) = code {
                            let key = normalize_code(c);
                            if looks_like_code(&key) {
                                g.code_index.insert(key, parsed);
                            }
                        }
                    }
                }
            }
        }
        if visual {
            self.publish_peers().await;
            self.publish_contacts().await;
        }
        if self.inner.lock().await.config.relay_enabled && ttl > 1 {
            let relay = MeshPacket::Presence {
                node_id,
                name,
                seq,
                ts,
                addr,
                code,
                ttl: ttl - 1,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    async fn ingest_ack(
        &self,
        id: Uuid,
        from: Uuid,
        to: Uuid,
        status: DeliveryStatus,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        if to == our {
            let _ = self.inner.lock().await.storage.remove_outbox(id);
            let _ = self
                .inner
                .lock()
                .await
                .storage
                .set_message_status(id, status);
            let _ = self.ui_tx.send(UiOut::MessageStatus { id, status });
            return Ok(());
        }
        if self.inner.lock().await.config.relay_enabled && ttl > 1 {
            let relay = MeshPacket::Ack {
                id,
                from,
                to,
                status,
                ttl: ttl - 1,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    fn dest_reachable_locked(g: &Inner, dest: Option<Uuid>) -> bool {
        if g.peers.is_empty() {
            return false;
        }
        match dest {
            None => true,
            Some(id) => g.peers.contains_key(&id) || g.config.relay_enabled,
        }
    }

    async fn enqueue_and_flush(&self, packet: MeshPacket, to: Option<Uuid>) -> Result<()> {
        let now = now_ts();
        let id = match &packet {
            MeshPacket::Chat { id, .. } | MeshPacket::FileOffer { id, .. } => *id,
            _ => return Ok(()),
        };
        {
            let mut g = self.inner.lock().await;
            g.storage.enqueue_outbox(OutboxItem {
                id,
                to,
                packet,
                attempts: 0,
                next_retry: now,
                created: now,
            })?;
        }
        self.ensure_dial_for(to).await;
        self.flush_outbox().await;
        // Outbound connect is async — retry flush shortly after dial.
        let n = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(900)).await;
            n.flush_outbox().await;
        });
        Ok(())
    }

    /// Dial last-known address for a DM target (or all known addrs for broadcast).
    async fn ensure_dial_for(&self, to: Option<Uuid>) {
        let jobs: Vec<(SocketAddr, &'static str)> = {
            let g = self.inner.lock().await;
            match to {
                Some(id) if g.peers.contains_key(&id) => Vec::new(),
                Some(id) => {
                    let mut addrs = Vec::new();
                    if let Some(hint) = g.presence.get(&id) {
                        if let Some(ref a) = hint.addr {
                            if let Ok(parsed) = a.parse::<SocketAddr>() {
                                addrs.push((parsed, "presence"));
                            }
                        }
                    }
                    if let Some(c) = g.storage.contacts().into_iter().find(|(cid, _)| *cid == id) {
                        if let Some(ref a) = c.1.last_addr {
                            if let Ok(parsed) = a.parse::<SocketAddr>() {
                                if !addrs.iter().any(|(x, _)| x == &parsed) {
                                    addrs.push((parsed, "contact"));
                                }
                            }
                        }
                    }
                    addrs
                }
                None => g
                    .remembered_addrs
                    .iter()
                    .map(|(a, s)| (*a, *s))
                    .chain(
                        g.storage
                            .known_peer_addrs()
                            .into_iter()
                            .filter_map(|a| a.parse().ok().map(|p| (p, "contact"))),
                    )
                    .collect(),
            }
        };
        for (addr, source) in jobs {
            self.connect_peer(addr, source).await;
        }
    }

    async fn flush_outbox(&self) {
        let now = now_ts();
        let due = {
            let g = self.inner.lock().await;
            g.storage.due_outbox(now)
        };
        for item in due {
            let reachable = {
                let g = self.inner.lock().await;
                Self::dest_reachable_locked(&g, item.to)
            };
            if !reachable {
                continue;
            }
            let updated = {
                let mut g = self.inner.lock().await;
                match g.storage.mark_outbox_attempt(item.id, now) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };
            let Some(updated) = updated else { continue };
            if updated.attempts > MAX_OUTBOX_ATTEMPTS {
                let _ = self.inner.lock().await.storage.remove_outbox(updated.id);
                let _ = self
                    .inner
                    .lock()
                    .await
                    .storage
                    .set_message_status(updated.id, DeliveryStatus::Failed);
                let _ = self.ui_tx.send(UiOut::MessageStatus {
                    id: updated.id,
                    status: DeliveryStatus::Failed,
                });
                continue;
            }
            self.relay(&updated.packet, None).await;
            let _ = self
                .inner
                .lock()
                .await
                .storage
                .set_message_status(updated.id, DeliveryStatus::Sent);
            let _ = self.ui_tx.send(UiOut::MessageStatus {
                id: updated.id,
                status: DeliveryStatus::Sent,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_file_offer(
        &self,
        id: Uuid,
        from: Uuid,
        from_name: String,
        to: Option<Uuid>,
        group_id: Option<Uuid>,
        file_id: Uuid,
        file_name: String,
        size: u64,
        sha256: String,
        ttl: u8,
        ts: i64,
        via: Option<Uuid>,
    ) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            if !g.seen.insert(id) {
                return Ok(());
            }
        }

        let (our_id, accept_broadcast, relay_enabled, member) = {
            let g = self.inner.lock().await;
            let member = match group_id {
                Some(gid) => {
                    g.storage.is_group_member(gid, g.identity.node_id) || from == g.identity.node_id
                }
                None => true,
            };
            (
                g.identity.node_id,
                g.config.accept_broadcast,
                g.config.relay_enabled,
                member,
            )
        };
        let local = self.inner.lock().await.storage.find_file(file_id).is_some();
        let for_us = if group_id.is_some() {
            member
        } else {
            match to {
                None => accept_broadcast || from == our_id,
                Some(id) => id == our_id || from == our_id,
            }
        };

        if for_us {
            {
                let mut g = self.inner.lock().await;
                let offer = CachedOffer {
                    id,
                    from,
                    from_name: from_name.clone(),
                    to,
                    group_id,
                    file_id,
                    file_name: file_name.clone(),
                    size,
                    sha256: sha256.clone(),
                    ts,
                };
                g.offers.insert(file_id, offer.clone());
                let _ = g
                    .storage
                    .upsert_file_offer(crate::protocol::StoredFileOffer {
                        id: offer.id,
                        from: offer.from,
                        from_name: offer.from_name.clone(),
                        to: offer.to,
                        group_id: offer.group_id,
                        file_id: offer.file_id,
                        file_name: offer.file_name.clone(),
                        size: offer.size,
                        sha256: offer.sha256.clone(),
                        ts: offer.ts,
                    });
                while g.offers.len() > MAX_CACHED_OFFERS {
                    if let Some(oldest) = g.offers.keys().next().copied() {
                        g.offers.remove(&oldest);
                    } else {
                        break;
                    }
                }
            }
            let _ = self.ui_tx.send(UiOut::FileAvailable {
                id,
                from,
                from_name: from_name.clone(),
                to,
                group_id,
                file_id,
                file_name: file_name.clone(),
                size,
                sha256: sha256.clone(),
                ts,
                local,
            });
            if from != our_id && to == Some(our_id) && group_id.is_none() {
                let ack = MeshPacket::Ack {
                    id,
                    from: our_id,
                    to: from,
                    status: DeliveryStatus::Delivered,
                    ttl: 8,
                };
                self.relay(&ack, None).await;
            }
        }

        let local_origin = via.is_none() && from == our_id;
        if relay_enabled && ttl > 1 && !local_origin {
            let relay = MeshPacket::FileOffer {
                id,
                from,
                from_name,
                to,
                group_id,
                file_id,
                file_name,
                size,
                sha256,
                ttl: ttl - 1,
                ts,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    async fn ingest_file_request(
        &self,
        id: Uuid,
        file_id: Uuid,
        from: Uuid,
        to: Uuid,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            if !g.seen.insert(id) {
                return Ok(());
            }
        }
        let (our_id, relay_enabled) = {
            let g = self.inner.lock().await;
            (g.identity.node_id, g.config.relay_enabled)
        };
        if to == our_id {
            if let Some(path) = self.inner.lock().await.storage.find_file(file_id) {
                if let Ok(bytes) = std::fs::read(&path) {
                    let _ = self.push_file_to(from, file_id, &bytes).await;
                }
            }
        } else if relay_enabled && ttl > 1 {
            let relay = MeshPacket::FileRequest {
                id,
                file_id,
                from,
                to,
                ttl: ttl - 1,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_file_chunk(
        &self,
        id: Uuid,
        file_id: Uuid,
        from: Uuid,
        to: Uuid,
        offset: u64,
        data_b64: String,
        done: bool,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            if !g.seen.insert(id) {
                return Ok(());
            }
        }
        let (our_id, relay_enabled, max_file) = {
            let g = self.inner.lock().await;
            (
                g.identity.node_id,
                g.config.relay_enabled,
                g.config.max_file_bytes,
            )
        };
        if to != our_id {
            if relay_enabled && ttl > 1 {
                let pkt = MeshPacket::FileChunk {
                    id,
                    file_id,
                    from,
                    to,
                    offset,
                    data_b64,
                    done,
                    ttl: ttl - 1,
                };
                self.relay(&pkt, via).await;
            }
            return Ok(());
        }

        let bytes = B64
            .decode(data_b64.as_bytes())
            .context("decode file chunk")?;
        if bytes.len() > max_file {
            return Err(anyhow!("file chunk exceeds max_file_bytes"));
        }
        let path = {
            let g = self.inner.lock().await;
            let tmp = g.storage.files_dir().join(format!("{file_id}.part"));
            if offset == 0 {
                std::fs::write(&tmp, &bytes)?;
            } else {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&tmp)?;
                f.write_all(&bytes)?;
            }
            tmp
        };

        if done {
            let final_bytes = std::fs::read(&path)?;
            if final_bytes.len() > max_file {
                let _ = std::fs::remove_file(&path);
                return Err(anyhow!("assembled file exceeds max_file_bytes"));
            }
            let offer = self.inner.lock().await.offers.get(&file_id).cloned();
            if let Some(ref offer) = offer {
                let mut h = Sha256::new();
                h.update(&final_bytes);
                let got = format!("{:x}", h.finalize());
                if got != offer.sha256 {
                    let _ = std::fs::remove_file(&path);
                    return Err(anyhow!("file sha256 mismatch"));
                }
            }
            let file_name = offer
                .as_ref()
                .map(|o| o.file_name.clone())
                .unwrap_or_else(|| format!("received-{file_id}"));
            let final_path = {
                let g = self.inner.lock().await;
                g.storage.store_file(file_id, &file_name, &final_bytes)?
            };
            let _ = std::fs::remove_file(&path);
            if let Some(offer) = offer {
                let _ = self.ui_tx.send(UiOut::FileAvailable {
                    id: offer.id,
                    from: offer.from,
                    from_name: offer.from_name,
                    to: offer.to,
                    group_id: offer.group_id,
                    file_id: offer.file_id,
                    file_name: offer.file_name.clone(),
                    size: offer.size,
                    sha256: offer.sha256,
                    ts: offer.ts,
                    local: true,
                });
            }
            let _ = self.ui_tx.send(UiOut::FileReceived {
                file_id,
                file_name,
                path: final_path.display().to_string(),
                size: final_bytes.len() as u64,
            });
            self.announce_status(format!("file received ({})", final_bytes.len()))
                .await;
        }
        Ok(())
    }

    async fn relay(&self, packet: &MeshPacket, except: Option<Uuid>) {
        let line = match serde_json::to_string(packet) {
            Ok(s) => s,
            Err(_) => return,
        };
        let targets: Vec<tokio::sync::mpsc::Sender<String>> = {
            let g = self.inner.lock().await;
            g.peers
                .iter()
                .filter(|(id, _)| Some(**id) != except)
                .map(|(_, p)| p.tx.clone())
                .collect()
        };
        for tx in targets {
            let _ = tx.send(line.clone()).await;
        }
    }

    async fn announce_name(&self) {
        let hello = {
            let g = self.inner.lock().await;
            let token = g.config.mesh_token.trim();
            MeshPacket::Hello {
                node_id: g.identity.node_id,
                name: g.identity.name.clone(),
                tcp_port: self.bind_mesh.1,
                version: PROTOCOL_VERSION,
                token: if token.is_empty() {
                    None
                } else {
                    Some(token.to_string())
                },
            }
        };
        self.relay(&hello, None).await;
    }

    pub async fn send_chat(
        &self,
        body: String,
        to: Option<Uuid>,
        group_id: Option<Uuid>,
        reply_to: Option<Uuid>,
    ) -> Result<()> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Err(anyhow!("empty message"));
        }
        let (ttl, max_body) = {
            let g = self.inner.lock().await;
            (g.config.ttl_default, g.config.max_body_bytes)
        };
        if body.len() > max_body {
            return Err(anyhow!("message too large"));
        }
        if let Some(gid) = group_id {
            let g = self.inner.lock().await;
            if !g.storage.is_group_member(gid, g.identity.node_id) {
                return Err(anyhow!("not a member of this group"));
            }
        }

        let (id, from, from_name, ts) = {
            let g = self.inner.lock().await;
            (
                Uuid::new_v4(),
                g.identity.node_id,
                g.identity.name.clone(),
                now_ts(),
            )
        };

        let dest = if group_id.is_some() { None } else { to };
        let (reply_author, reply_snippet) = if let Some(rid) = reply_to {
            let parent = self.inner.lock().await.storage.message(rid);
            match parent {
                Some(p) => {
                    let snip: String = p.body.chars().take(120).collect();
                    (Some(p.from_name), Some(snip))
                }
                None => (None, None),
            }
        } else {
            (None, None)
        };
        let packet = MeshPacket::Chat {
            id,
            from,
            from_name: from_name.clone(),
            to: dest,
            group_id,
            body: body.clone(),
            ttl,
            ts,
            reply_to,
            reply_author: reply_author.clone(),
            reply_snippet: reply_snippet.clone(),
            edited: false,
        };
        self.ingest_chat(
            id,
            from,
            from_name,
            dest,
            group_id,
            body,
            ttl,
            ts,
            reply_to,
            reply_author,
            reply_snippet,
            false,
            None,
        )
        .await?;
        self.enqueue_and_flush(packet, dest).await
    }

    pub async fn send_file(
        &self,
        file_name: String,
        data_b64: String,
        to: Option<Uuid>,
        group_id: Option<Uuid>,
    ) -> Result<()> {
        let bytes = B64.decode(data_b64.as_bytes()).context("decode file")?;
        let (ttl, max_file) = {
            let g = self.inner.lock().await;
            (g.config.ttl_default, g.config.max_file_bytes)
        };
        if bytes.len() > max_file {
            return Err(anyhow!("file too large (max {max_file} bytes)"));
        }
        if let Some(gid) = group_id {
            let g = self.inner.lock().await;
            if !g.storage.is_group_member(gid, g.identity.node_id) {
                return Err(anyhow!("not a member of this group"));
            }
        }

        let sha256 = {
            let mut h = Sha256::new();
            h.update(&bytes);
            format!("{:x}", h.finalize())
        };

        let file_id = Uuid::new_v4();
        let path = {
            let g = self.inner.lock().await;
            g.storage.store_file(file_id, &file_name, &bytes)?
        };

        let (offer_id, from, from_name, ts) = {
            let g = self.inner.lock().await;
            (
                Uuid::new_v4(),
                g.identity.node_id,
                g.identity.name.clone(),
                now_ts(),
            )
        };

        let dest = if group_id.is_some() { None } else { to };
        let packet = MeshPacket::FileOffer {
            id: offer_id,
            from,
            from_name: from_name.clone(),
            to: dest,
            group_id,
            file_id,
            file_name: file_name.clone(),
            size: bytes.len() as u64,
            sha256: sha256.clone(),
            ttl,
            ts,
        };
        self.ingest_file_offer(
            offer_id,
            from,
            from_name,
            dest,
            group_id,
            file_id,
            file_name.clone(),
            bytes.len() as u64,
            sha256,
            ttl,
            ts,
            None,
        )
        .await?;
        let _ = self.enqueue_and_flush(packet, dest).await;

        if let Some(dest) = dest {
            self.push_file_to(dest, file_id, &bytes).await?;
        }

        self.announce_status(format!(
            "file stored locally: {} ({})",
            path.display(),
            bytes.len()
        ))
        .await;
        Ok(())
    }

    async fn push_file_to(&self, to: Uuid, file_id: Uuid, bytes: &[u8]) -> Result<()> {
        let (from, ttl) = {
            let g = self.inner.lock().await;
            (g.identity.node_id, g.config.ttl_default)
        };
        let pkt = MeshPacket::FileChunk {
            id: Uuid::new_v4(),
            file_id,
            from,
            to,
            offset: 0,
            data_b64: B64.encode(bytes),
            done: true,
            ttl,
        };
        self.relay(&pkt, None).await;
        Ok(())
    }

    pub async fn request_file(&self, file_id: Uuid, from: Uuid) -> Result<()> {
        if let Some(path) = self.inner.lock().await.storage.find_file(file_id) {
            let _ = self.ui_tx.send(UiOut::FileReceived {
                file_id,
                file_name: path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.display().to_string(),
                size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            });
            return Ok(());
        }
        let (id, us, ttl) = {
            let g = self.inner.lock().await;
            (Uuid::new_v4(), g.identity.node_id, g.config.ttl_default)
        };
        self.ingest_file_request(id, file_id, us, from, ttl, None)
            .await?;
        self.announce_status("file request sent").await;
        Ok(())
    }

    pub async fn set_name(&self, name: String) -> Result<()> {
        let name = sanitize_name(&name);
        {
            let mut g = self.inner.lock().await;
            g.identity.name = name.clone();
            g.identity.save(&self.data_dir)?;
        }
        self.publish_hello().await;
        self.announce_name().await;
        self.publish_config().await;
        Ok(())
    }

    async fn set_avatar(&self, data_b64: String, mime: String) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let ts = now_ts();
        if data_b64.trim().is_empty() {
            self.inner.lock().await.storage.clear_avatar(our)?;
            let _ = self.ui_tx.send(UiOut::Avatar {
                node_id: our,
                mime: String::new(),
                data_b64: String::new(),
            });
            let packet = MeshPacket::Avatar {
                node_id: our,
                mime: String::new(),
                data_b64: String::new(),
                ts,
                ttl: AVATAR_TTL,
            };
            self.relay(&packet, None).await;
            self.announce_status("avatar cleared").await;
            return Ok(());
        }
        let bytes = B64.decode(data_b64.as_bytes()).context("decode avatar")?;
        if bytes.len() > Storage::MAX_AVATAR_BYTES {
            return Err(anyhow!("avatar too large (max 48 KB)"));
        }
        let mime = if mime.trim().is_empty() {
            "image/jpeg".into()
        } else {
            mime
        };
        self.inner
            .lock()
            .await
            .storage
            .set_avatar(our, &mime, &bytes)?;
        let encoded = B64.encode(&bytes);
        let _ = self.ui_tx.send(UiOut::Avatar {
            node_id: our,
            mime: mime.clone(),
            data_b64: encoded.clone(),
        });
        let packet = MeshPacket::Avatar {
            node_id: our,
            mime,
            data_b64: encoded,
            ts,
            ttl: AVATAR_TTL,
        };
        self.relay(&packet, None).await;
        self.announce_status("avatar updated").await;
        Ok(())
    }

    async fn ingest_avatar(
        &self,
        node_id: Uuid,
        mime: String,
        data_b64: String,
        _ts: i64,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        // Dedup by content hash so floods don't rewrite disk endlessly.
        let dedup_key = {
            let mut h = Sha256::new();
            h.update(node_id.as_bytes());
            h.update(data_b64.as_bytes());
            let digest = h.finalize();
            Uuid::from_bytes([
                digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6],
                digest[7], digest[8], digest[9], digest[10], digest[11], digest[12], digest[13],
                digest[14], digest[15],
            ])
        };
        {
            let mut g = self.inner.lock().await;
            if !g.seen.insert(dedup_key) {
                return Ok(());
            }
        }

        if data_b64.trim().is_empty() {
            if node_id != our {
                self.inner.lock().await.storage.clear_avatar(node_id)?;
            }
            let _ = self.ui_tx.send(UiOut::Avatar {
                node_id,
                mime: String::new(),
                data_b64: String::new(),
            });
        } else {
            let bytes = match B64.decode(data_b64.as_bytes()) {
                Ok(b) if !b.is_empty() && b.len() <= Storage::MAX_AVATAR_BYTES => b,
                _ => return Ok(()),
            };
            self.inner
                .lock()
                .await
                .storage
                .set_avatar(node_id, &mime, &bytes)?;
            let _ = self.ui_tx.send(UiOut::Avatar {
                node_id,
                mime: mime.clone(),
                data_b64: B64.encode(&bytes),
            });
        }

        let local_origin = via.is_none() && node_id == our;
        if !local_origin && ttl > 1 {
            let relay = MeshPacket::Avatar {
                node_id,
                mime,
                data_b64,
                ts: now_ts(),
                ttl: ttl - 1,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    async fn share_our_avatar(&self) {
        let (our, mime, bytes) = {
            let g = self.inner.lock().await;
            let our = g.identity.node_id;
            match g.storage.get_avatar(our) {
                Some((mime, bytes)) => (our, mime, bytes),
                None => return,
            }
        };
        let packet = MeshPacket::Avatar {
            node_id: our,
            mime,
            data_b64: B64.encode(&bytes),
            ts: now_ts(),
            ttl: AVATAR_TTL,
        };
        self.relay(&packet, None).await;
    }

    async fn complete_onboarding(&self, name: String) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            g.identity.complete_onboarding(&self.data_dir, &name)?;
        }
        self.publish_hello().await;
        self.announce_name().await;
        self.publish_config().await;
        self.announce_status("node registered").await;
        Ok(())
    }

    async fn set_config(&self, patch: ConfigPatch) -> Result<()> {
        let restart = {
            let mut g = self.inner.lock().await;
            let restart = g.config.apply_patch(patch)?;
            g.config.save(&self.data_dir)?;
            let cap = g.config.max_seen_ids;
            g.seen.set_cap(cap);
            if restart {
                g.restart_required = true;
            }
            restart
        };
        self.publish_config().await;
        if restart {
            self.announce_status("bind/port change saved — restart mrless to apply")
                .await;
        } else {
            self.announce_status("config saved").await;
        }
        Ok(())
    }

    async fn add_manual_peer(&self, addr: String) -> Result<()> {
        let (parsed, token) = if looks_like_code(&addr) {
            (self.resolve_invite_code(&addr).await?, None)
        } else {
            parse_peer_target(&addr)?
        };
        let stored = parsed.to_string();
        {
            let mut g = self.inner.lock().await;
            if !g.config.manual_peers.iter().any(|x| x == &stored) {
                g.config.manual_peers.push(stored.clone());
            }
            if let Some(t) = token {
                if !t.is_empty() && g.config.mesh_token.trim().is_empty() {
                    g.config.mesh_token = t;
                }
            }
            g.config.save(&self.data_dir)?;
        }
        self.connect_peer(parsed, "manual").await;
        // Give TCP a moment, then push queued DMs.
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.flush_outbox().await;
        self.publish_config().await;
        self.announce_status(format!("manual peer {stored}")).await;
        Ok(())
    }

    async fn remove_manual_peer(&self, addr: String) -> Result<()> {
        let parsed = addr.parse::<SocketAddr>().ok();
        let drop_ids: Vec<Uuid> = {
            let mut g = self.inner.lock().await;
            g.config.manual_peers.retain(|x| x != &addr);
            g.config.save(&self.data_dir)?;
            g.peers
                .iter()
                .filter(|(_, p)| {
                    parsed.map(|a| p.addr == a).unwrap_or(false) || p.addr.to_string() == addr
                })
                .map(|(id, _)| *id)
                .collect()
        };
        self.publish_config().await;
        for id in drop_ids {
            self.drop_peer(id, "manual peer removed").await;
        }
        Ok(())
    }

    pub async fn reconnect_peers(&self) -> Result<()> {
        let jobs: Vec<(SocketAddr, &'static str)> = {
            let mut g = self.inner.lock().await;
            for addr in g.config.parse_manual_addrs() {
                g.remembered_addrs.entry(addr).or_insert("manual");
            }
            g.known_addrs.clear();
            g.remembered_addrs.iter().map(|(a, s)| (*a, *s)).collect()
        };
        for (addr, source) in jobs {
            self.connect_peer(addr, source).await;
        }
        self.flush_outbox().await;
        self.announce_status("reconnecting peers").await;
        Ok(())
    }

    async fn resend_message(&self, id: Uuid) -> Result<()> {
        let existing = self.inner.lock().await.storage.outbox_item(id);
        if let Some(item) = existing {
            let mut g = self.inner.lock().await;
            g.storage.remove_outbox(id)?;
            g.storage.enqueue_outbox(OutboxItem {
                id,
                to: item.to,
                packet: item.packet,
                attempts: 0,
                next_retry: now_ts(),
                created: now_ts(),
            })?;
            g.storage.set_message_status(id, DeliveryStatus::Queued)?;
        } else if let Some(msg) = self.inner.lock().await.storage.message(id) {
            let ttl = self.inner.lock().await.config.ttl_default;
            let packet = MeshPacket::Chat {
                id: msg.id,
                from: msg.from,
                from_name: msg.from_name,
                to: msg.to,
                group_id: msg.group_id,
                body: msg.body,
                ttl,
                ts: msg.ts,
                reply_to: msg.reply_to,
                reply_author: msg.reply_author,
                reply_snippet: msg.reply_snippet,
                edited: msg.edited,
            };
            let mut g = self.inner.lock().await;
            g.storage.enqueue_outbox(OutboxItem {
                id,
                to: msg.to,
                packet,
                attempts: 0,
                next_retry: now_ts(),
                created: now_ts(),
            })?;
            g.storage.set_message_status(id, DeliveryStatus::Queued)?;
        } else {
            return Err(anyhow!("message not found"));
        }
        let _ = self.ui_tx.send(UiOut::MessageStatus {
            id,
            status: DeliveryStatus::Queued,
        });
        self.flush_outbox().await;
        Ok(())
    }

    fn invite_url(&self, g: &Inner) -> String {
        let listen = advertise_addr(self.bind_mesh.0, self.bind_mesh.1);
        let token = g.config.mesh_token.trim();
        let raw = if token.is_empty() {
            format!("{}|{}|{}", g.identity.node_id, g.identity.name, listen)
        } else {
            format!(
                "{}|{}|{}|{token}",
                g.identity.node_id, g.identity.name, listen
            )
        };
        format!("mrless://peer/{}", B64.encode(raw.as_bytes()))
    }

    fn invite_code_for(&self, g: &Inner) -> String {
        let existing = g.config.invite_code.trim();
        if looks_like_code(existing) {
            return normalize_code(existing);
        }
        invite_code::random_code()
    }

    async fn ensure_invite_code_persisted(&self) {
        let mut g = self.inner.lock().await;
        if looks_like_code(&g.config.invite_code) {
            return;
        }
        g.config.invite_code = invite_code::random_code();
        let _ = g.config.save(&self.data_dir);
    }

    async fn publish_invite(&self) {
        self.ensure_invite_code_persisted().await;
        let out = {
            let g = self.inner.lock().await;
            UiOut::Invite {
                url: self.invite_url(&g),
                code: self.invite_code_for(&g),
                listen: advertise_addr(self.bind_mesh.0, self.bind_mesh.1),
                node_id: g.identity.node_id,
            }
        };
        let _ = self.ui_tx.send(out);
    }

    /// Mint a new 8-char code and re-advertise listen address.
    async fn refresh_invite(&self) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            g.config.invite_code = invite_code::random_code();
            g.config.save(&self.data_dir)?;
        }
        self.publish_invite().await;
        self.publish_stats().await;
        self.flood_presence().await;
        self.announce_status("invite refreshed").await;
        Ok(())
    }

    async fn resolve_invite_code(&self, code: &str) -> Result<SocketAddr> {
        let n = normalize_code(code);
        // Legacy / offline: codes that embed 192.168 / 10.x / 100.x addresses.
        if let Ok(addr) = invite_code::decode_addr(&n) {
            return Ok(addr);
        }
        let found = {
            let g = self.inner.lock().await;
            if let Some(addr) = g.code_index.get(&n).copied() {
                Some(addr)
            } else {
                g.presence.values().find_map(|h| {
                    if h.code.as_deref().map(normalize_code).as_deref() == Some(n.as_str()) {
                        h.addr.as_ref()?.parse().ok()
                    } else {
                        None
                    }
                })
            }
        };
        if let Some(addr) = found {
            return Ok(addr);
        }
        Err(anyhow!(
            "invite code not found — keep their app open on the same Wi‑Fi, or use host:port"
        ))
    }

    fn group_packet(group: &StoredGroup, ttl: u8) -> MeshPacket {
        MeshPacket::GroupMeta {
            id: group.id,
            name: group.name.clone(),
            members: group.members.clone(),
            owner: group.owner,
            seq: group.seq,
            ts: group.updated,
            ttl,
        }
    }

    async fn flood_group(&self, group: &StoredGroup) {
        let ttl = self.inner.lock().await.config.ttl_default.max(2);
        let packet = Self::group_packet(group, ttl);
        self.relay(&packet, None).await;
    }

    async fn announce_owned_groups(&self) {
        let (our, ttl, groups) = {
            let g = self.inner.lock().await;
            (
                g.identity.node_id,
                g.config.ttl_default.max(2),
                g.storage.groups(),
            )
        };
        for group in groups.into_iter().filter(|gr| gr.owner == our).take(8) {
            let packet = Self::group_packet(&group, ttl);
            self.relay(&packet, None).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_group_meta(
        &self,
        id: Uuid,
        name: String,
        members: Vec<Uuid>,
        owner: Uuid,
        seq: u64,
        ts: i64,
        ttl: u8,
        via: Option<Uuid>,
    ) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let we_belong = members.contains(&our);
        let mut changed = false;
        let mut ignore_local = false;
        {
            let mut g = self.inner.lock().await;
            // Ignore stale meta after we left/deleted — otherwise peers re-add the chat.
            if let Some(left_seq) = g.storage.left_group_seq(id) {
                if seq <= left_seq {
                    ignore_local = true;
                } else {
                    // Newer than our leave — clear tombstone (re-invite or dissolve).
                    let _ = g.storage.clear_left_group(id);
                }
            }
            if !ignore_local {
                if we_belong {
                    changed = g.storage.upsert_group(StoredGroup {
                        id,
                        name: name.clone(),
                        members: members.clone(),
                        owner,
                        seq,
                        updated: ts,
                    })?;
                } else if g.storage.group(id).is_some() {
                    changed = g.storage.remove_group(id)?;
                }
            }
        }
        if changed {
            self.publish_groups().await;
        }
        let local_origin = via.is_none();
        if self.inner.lock().await.config.relay_enabled && ttl > 1 && !local_origin {
            let relay = MeshPacket::GroupMeta {
                id,
                name,
                members,
                owner,
                seq,
                ts,
                ttl: ttl - 1,
            };
            self.relay(&relay, via).await;
        }
        Ok(())
    }

    async fn create_group(&self, name: String, mut members: Vec<Uuid>) -> Result<()> {
        let name = sanitize_group_name(&name);
        if name.is_empty() {
            return Err(anyhow!("group name required"));
        }
        let (our, ts) = {
            let g = self.inner.lock().await;
            (g.identity.node_id, now_ts())
        };
        members.push(our);
        members.sort();
        members.dedup();
        let group = StoredGroup {
            id: Uuid::new_v4(),
            name,
            members,
            owner: our,
            seq: 1,
            updated: ts,
        };
        {
            let mut g = self.inner.lock().await;
            g.storage.upsert_group(group.clone())?;
        }
        self.publish_groups().await;
        self.flood_group(&group).await;
        self.announce_status(format!("group created: {}", group.name))
            .await;
        Ok(())
    }

    async fn update_group(
        &self,
        id: Uuid,
        name: Option<String>,
        members: Option<Vec<Uuid>>,
    ) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let existing = self
            .inner
            .lock()
            .await
            .storage
            .group(id)
            .ok_or_else(|| anyhow!("group not found"))?;
        if existing.owner != our {
            return Err(anyhow!("only the owner can edit this group"));
        }
        let mut next = existing;
        if let Some(n) = name {
            let n = sanitize_group_name(&n);
            if n.is_empty() {
                return Err(anyhow!("group name required"));
            }
            next.name = n;
        }
        if let Some(mut m) = members {
            m.push(our);
            m.sort();
            m.dedup();
            next.members = m;
        }
        next.seq += 1;
        next.updated = now_ts();
        {
            let mut g = self.inner.lock().await;
            g.storage.upsert_group(next.clone())?;
        }
        self.publish_groups().await;
        self.flood_group(&next).await;
        Ok(())
    }

    async fn leave_group(&self, id: Uuid) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let existing = self
            .inner
            .lock()
            .await
            .storage
            .group(id)
            .ok_or_else(|| anyhow!("group not found"))?;
        // Owner leaving with no one else → dissolve for everyone.
        if existing.owner == our && existing.members.iter().all(|m| *m == our) {
            return self.delete_group(id).await;
        }
        // Owner leaving while others remain: transfer ownership to next member.
        let mut next = existing;
        next.seq += 1;
        next.updated = now_ts();
        next.members.retain(|m| *m != our);
        if next.owner == our {
            if let Some(new_owner) = next.members.first().copied() {
                next.owner = new_owner;
            }
        }
        {
            let mut g = self.inner.lock().await;
            g.storage.leave_group_local(id, next.seq)?;
        }
        self.publish_groups().await;
        self.flood_group(&next).await;
        self.announce_status("left group").await;
        Ok(())
    }

    /// Owner dissolves the group for all members.
    async fn delete_group(&self, id: Uuid) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let existing = self
            .inner
            .lock()
            .await
            .storage
            .group(id)
            .ok_or_else(|| anyhow!("group not found"))?;
        if existing.owner != our {
            return Err(anyhow!("only the owner can delete this group"));
        }
        let mut next = existing;
        next.seq += 1;
        next.updated = now_ts();
        next.members.clear();
        {
            let mut g = self.inner.lock().await;
            g.storage.leave_group_local(id, next.seq)?;
        }
        self.publish_groups().await;
        self.flood_group(&next).await;
        self.announce_status("group deleted").await;
        Ok(())
    }

    async fn publish_storage(&self) {
        let out = {
            let g = self.inner.lock().await;
            let our = g.identity.node_id;
            let (messages_bytes, files_bytes, other_bytes) = g.storage.usage_bytes();
            let files = g.storage.list_cached_files();
            let chats = g.storage.chat_storage_summaries(our);
            let message_count = g.storage.message_count();
            UiOut::StorageUsage {
                total_bytes: messages_bytes + files_bytes + other_bytes,
                messages_bytes,
                files_bytes,
                other_bytes,
                message_count,
                files,
                chats,
            }
        };
        let _ = self.ui_tx.send(out);
    }

    async fn delete_chat_messages(&self, chat_key: String) -> Result<()> {
        let our = self.inner.lock().await.identity.node_id;
        let (removed, drop_files) = {
            let mut g = self.inner.lock().await;
            let removed = g.storage.delete_messages_for_chat(&chat_key, our)?;
            let mut drop_files = Vec::new();
            let offers: Vec<_> = g.offers.values().cloned().collect();
            for o in offers {
                let drop = if chat_key == "lan" {
                    o.group_id.is_none() && o.to.is_none()
                } else if let Some(rest) = chat_key.strip_prefix("g:") {
                    o.group_id.map(|id| id.to_string()).as_deref() == Some(rest)
                } else if let Ok(peer) = Uuid::parse_str(&chat_key) {
                    o.group_id.is_none()
                        && o.to.is_some()
                        && ((o.from == our && o.to == Some(peer))
                            || (o.from == peer && (o.to == Some(our) || o.to.is_none())))
                } else {
                    false
                };
                if drop {
                    g.offers.remove(&o.file_id);
                    let _ = g.storage.remove_file_offer(o.file_id);
                    drop_files.push(o.file_id);
                }
            }
            (removed, drop_files)
        };
        let _ = self.ui_tx.send(UiOut::MessagesDeleted {
            chat_key: chat_key.clone(),
        });
        for file_id in drop_files {
            let _ = self.ui_tx.send(UiOut::FileOfferDeleted { file_id });
        }
        self.publish_storage().await;
        self.announce_status(format!("deleted {removed} messages"))
            .await;
        Ok(())
    }

    async fn delete_cached_file(&self, file_id: Uuid) -> Result<()> {
        let ok = {
            let g = self.inner.lock().await;
            g.storage.delete_file(file_id)?
        };
        if !ok {
            return Err(anyhow!("file not in cache"));
        }
        // Mark offers non-local in UI by re-emitting availability if known.
        if let Some(offer) = self.inner.lock().await.offers.get(&file_id).cloned() {
            let _ = self.ui_tx.send(UiOut::FileAvailable {
                id: offer.id,
                from: offer.from,
                from_name: offer.from_name,
                to: offer.to,
                group_id: offer.group_id,
                file_id: offer.file_id,
                file_name: offer.file_name,
                size: offer.size,
                sha256: offer.sha256,
                ts: offer.ts,
                local: false,
            });
        }
        let _ = self.ui_tx.send(UiOut::FileDeleted { file_id });
        self.publish_storage().await;
        Ok(())
    }

    async fn purge_file_cache(&self) -> Result<()> {
        let n = {
            let g = self.inner.lock().await;
            g.storage.purge_file_cache()?
        };
        let _ = self.ui_tx.send(UiOut::FileCachePurged);
        self.publish_storage().await;
        self.announce_status(format!("purged {n} cached files"))
            .await;
        Ok(())
    }

    async fn delete_one_message(&self, id: Uuid) -> Result<()> {
        let ok = {
            let mut g = self.inner.lock().await;
            g.storage.delete_message(id)?
        };
        if !ok {
            return Err(anyhow!("message not found"));
        }
        let _ = self.ui_tx.send(UiOut::MessageDeleted { id });
        self.publish_storage().await;
        Ok(())
    }

    async fn edit_message(&self, id: Uuid, body: String) -> Result<()> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Err(anyhow!("empty message"));
        }
        let (our_id, msg, ttl) = {
            let g = self.inner.lock().await;
            (
                g.identity.node_id,
                g.storage.message(id),
                g.config.ttl_default,
            )
        };
        let Some(msg) = msg else {
            return Err(anyhow!("message not found"));
        };
        if msg.from != our_id {
            return Err(anyhow!("can only edit your messages"));
        }
        // Keep original message time; do not re-queue through outbox.
        let ts = msg.ts;
        self.ingest_chat(
            msg.id,
            msg.from,
            msg.from_name,
            msg.to,
            msg.group_id,
            body,
            ttl,
            ts,
            msg.reply_to,
            msg.reply_author,
            msg.reply_snippet,
            true,
            None,
        )
        .await?;
        Ok(())
    }

    async fn forward_message(
        &self,
        id: Uuid,
        to: Option<Uuid>,
        group_id: Option<Uuid>,
    ) -> Result<()> {
        let msg = self
            .inner
            .lock()
            .await
            .storage
            .message(id)
            .ok_or_else(|| anyhow!("message not found"))?;
        let body = msg.body;
        self.send_chat(body, to, group_id, None).await
    }

    async fn delete_file_offer(&self, file_id: Uuid) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            g.offers.remove(&file_id);
            let _ = g.storage.remove_file_offer(file_id)?;
        }
        let _ = self.ui_tx.send(UiOut::FileOfferDeleted { file_id });
        Ok(())
    }

    async fn export_cached_file(&self, file_id: Uuid) -> Result<()> {
        let path = {
            let g = self.inner.lock().await;
            g.storage
                .find_file(file_id)
                .ok_or_else(|| anyhow!("file not in cache"))?
        };
        let bytes = std::fs::read(&path).context("read cached file")?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.bin");
        let file_name = file_name
            .split_once('_')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| file_name.to_string());
        let data_b64 = B64.encode(&bytes);
        let _ = self.ui_tx.send(UiOut::FileExport {
            file_id,
            file_name,
            data_b64,
            size: bytes.len() as u64,
        });
        Ok(())
    }

    async fn reset_node(&self) -> Result<()> {
        {
            let mut g = self.inner.lock().await;
            g.storage.wipe()?;
            let ports = (
                g.config.mesh_port,
                g.config.ws_port,
                g.config.discovery_port,
            );
            let binds = (g.config.bind_mesh.clone(), g.config.bind_ws.clone());
            g.config = AppConfig::default();
            g.config.mesh_port = ports.0;
            g.config.ws_port = ports.1;
            g.config.discovery_port = ports.2;
            g.config.bind_mesh = binds.0;
            g.config.bind_ws = binds.1;
            g.config.save(&self.data_dir)?;
            g.identity.onboarded = false;
            g.identity.name = "node".into();
            g.identity.save(&self.data_dir)?;
            g.seen = SeenSet::new(g.config.max_seen_ids);
            g.offers.clear();
            g.presence.clear();
            g.code_index.clear();
            g.remembered_addrs.clear();
            g.last_peers_sig = None;
            g.last_contacts_sig = None;
            g.last_stats_sig = None;
        }
        let _ = self.ui_tx.send(UiOut::Cleared);
        self.publish_hello().await;
        self.publish_config().await;
        self.publish_groups().await;
        self.announce_status("local data wiped; node id kept").await;
        Ok(())
    }

    async fn run_discovery(&self) -> Result<()> {
        let sock = Arc::new(bind_discovery_socket(self.discovery_port)?);
        sock.set_broadcast(true)?;
        tracing::info!("discovery UDP on :{}", self.discovery_port);

        let recv_sock = sock.clone();
        let node = self.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                match recv_sock.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        let enabled = node.inner.lock().await.config.discovery_enabled;
                        if !enabled {
                            continue;
                        }
                        if let Some((port, code)) = parse_discovery(&buf[..n]) {
                            let target = SocketAddr::new(from.ip(), port);
                            if let Some(code) = code {
                                let key = normalize_code(&code);
                                if looks_like_code(&key) {
                                    node.inner.lock().await.code_index.insert(key, target);
                                }
                            }
                            node.connect_peer(target, "discovered").await;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("discovery recv: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        });

        loop {
            let (enabled, interval, mesh_port) = {
                let g = self.inner.lock().await;
                (
                    g.config.discovery_enabled,
                    g.config.discovery_interval_ms,
                    self.bind_mesh.1,
                )
            };
            tokio::time::sleep(Duration::from_millis(interval.max(250))).await;
            if !enabled {
                continue;
            }
            let (node_id, name, _) = self.identity_snapshot().await;
            self.ensure_invite_code_persisted().await;
            let code = {
                let g = self.inner.lock().await;
                self.invite_code_for(&g)
            };
            let payload = format!(
                "{}|{}|{}|{}|{}",
                std::str::from_utf8(DISCOVERY_MAGIC).unwrap(),
                node_id,
                name.replace('|', "_"),
                mesh_port,
                code
            );
            let _ = sock
                .send_to(payload.as_bytes(), ("255.255.255.255", self.discovery_port))
                .await;
        }
    }

    async fn run_mdns(&self) -> Result<()> {
        let mdns = mdns_sd::ServiceDaemon::new().context("mdns daemon")?;
        let (node_id, name, _) = self.identity_snapshot().await;
        let instance = format!("mrless-{}", short_id(node_id));
        let host = format!("{}.local.", instance);
        let port = self.bind_mesh.1;
        let mut props = HashMap::new();
        props.insert("id".to_string(), node_id.to_string());
        props.insert("name".to_string(), name);
        let info = mdns_sd::ServiceInfo::new(MDNS_SERVICE, &instance, &host, "", port, Some(props))
            .context("mdns service")?
            .enable_addr_auto();
        mdns.register(info).context("mdns register")?;
        tracing::info!("mDNS advertise {MDNS_SERVICE} as {instance}");

        let browse = mdns.browse(MDNS_SERVICE).context("mdns browse")?;
        loop {
            let enabled = self.inner.lock().await.config.discovery_enabled;
            match browse.recv_async().await {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                    if !enabled {
                        continue;
                    }
                    if info
                        .get_property_val_str("id")
                        .map(|id| id == node_id.to_string())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let port = info.get_port();
                    let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                    addrs.sort_by_key(|a| mdns_addr_rank(*a));
                    for addr in addrs {
                        if !is_usable_mesh_ip(addr) {
                            continue;
                        }
                        let target = SocketAddr::new(addr, port);
                        self.connect_peer(target, "mdns").await;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("mDNS browse: {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    async fn run_ws(&self) -> Result<()> {
        let addr = SocketAddr::new(self.bind_ws.0, self.bind_ws.1);
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind WS {addr}"))?;
        tracing::info!("UI WebSocket on ws://{}:{}", self.bind_ws.0, self.bind_ws.1);

        loop {
            let (stream, addr) = listener.accept().await?;
            let node = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node.handle_ws_client(stream).await {
                    tracing::debug!("ws {addr} ended: {e:#}");
                }
            });
        }
    }

    async fn handle_ws_client(&self, stream: TcpStream) -> Result<()> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut sink, mut stream) = ws.split();
        let mut ui_rx = self.ui_tx.subscribe();

        {
            let g = self.inner.lock().await;
            let hello = UiOut::Hello {
                node_id: g.identity.node_id,
                name: g.identity.name.clone(),
                mesh_port: self.bind_mesh.1,
                ws_port: self.bind_ws.1,
                onboarded: g.identity.onboarded,
            };
            sink.send(WsMessage::Text(serde_json::to_string(&hello)?.into()))
                .await?;
            if !g.identity.onboarded {
                let need = UiOut::NeedOnboarding {
                    node_id: g.identity.node_id,
                };
                sink.send(WsMessage::Text(serde_json::to_string(&need)?.into()))
                    .await?;
            }
            let cfg = UiOut::Config {
                config: g.config.clone(),
                restart_required: g.restart_required,
                display_name: g.identity.name.clone(),
                node_id: g.identity.node_id,
                onboarded: g.identity.onboarded,
            };
            sink.send(WsMessage::Text(serde_json::to_string(&cfg)?.into()))
                .await?;

            for msg in g.storage.history() {
                let out = UiOut::Message {
                    id: msg.id,
                    from: msg.from,
                    from_name: msg.from_name,
                    to: msg.to,
                    group_id: msg.group_id,
                    body: msg.body,
                    ts: msg.ts,
                    mine: msg.from == g.identity.node_id,
                    status: msg.status,
                    reply_to: msg.reply_to,
                    reply_author: msg.reply_author,
                    reply_snippet: msg.reply_snippet,
                    edited: msg.edited,
                };
                sink.send(WsMessage::Text(serde_json::to_string(&out)?.into()))
                    .await?;
            }
            let groups_out = UiOut::Groups {
                groups: g.storage.groups().into_iter().map(group_to_info).collect(),
            };
            sink.send(WsMessage::Text(serde_json::to_string(&groups_out)?.into()))
                .await?;
            let offers: Vec<CachedOffer> = g.offers.values().cloned().collect();
            let our_id = g.identity.node_id;
            let peers = Self::collect_peers_locked(&g);
            let contacts: Vec<ContactInfo> = g
                .storage
                .contacts()
                .into_iter()
                .map(|(id, c)| ContactInfo {
                    id,
                    name: c.name,
                    last_addr: c.last_addr,
                    last_seen: c.last_seen,
                })
                .collect();
            drop(g);
            for offer in offers {
                let local = self
                    .inner
                    .lock()
                    .await
                    .storage
                    .find_file(offer.file_id)
                    .is_some();
                let out = UiOut::FileAvailable {
                    id: offer.id,
                    from: offer.from,
                    from_name: offer.from_name,
                    to: offer.to,
                    group_id: offer.group_id,
                    file_id: offer.file_id,
                    file_name: offer.file_name,
                    size: offer.size,
                    sha256: offer.sha256,
                    ts: offer.ts,
                    local: local || offer.from == our_id,
                };
                sink.send(WsMessage::Text(serde_json::to_string(&out)?.into()))
                    .await?;
            }
            sink.send(WsMessage::Text(
                serde_json::to_string(&UiOut::Peers { peers })?.into(),
            ))
            .await?;
            sink.send(WsMessage::Text(
                serde_json::to_string(&UiOut::Contacts { contacts })?.into(),
            ))
            .await?;
            let avatar_ids = self.inner.lock().await.storage.list_avatar_ids();
            for id in avatar_ids {
                if let Some((mime, bytes)) = self.inner.lock().await.storage.get_avatar(id) {
                    let out = UiOut::Avatar {
                        node_id: id,
                        mime,
                        data_b64: B64.encode(&bytes),
                    };
                    sink.send(WsMessage::Text(serde_json::to_string(&out)?.into()))
                        .await?;
                }
            }
        }
        self.publish_stats().await;

        loop {
            tokio::select! {
                evt = ui_rx.recv() => {
                    match evt {
                        Ok(out) => {
                            sink.send(WsMessage::Text(serde_json::to_string(&out)?.into())).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                msg = stream.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            if text.len() > MAX_MESH_LINE {
                                let err = UiOut::Error {
                                    text: "websocket message too large".into(),
                                };
                                sink.send(WsMessage::Text(serde_json::to_string(&err)?.into()))
                                    .await?;
                                continue;
                            }
                            if let Err(e) = self.on_ui_text(&text).await {
                                let err = UiOut::Error { text: format!("{e:#}") };
                                sink.send(WsMessage::Text(serde_json::to_string(&err)?.into())).await?;
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(e.into()),
                    }
                }
            }
        }
        Ok(())
    }

    async fn on_ui_text(&self, text: &str) -> Result<()> {
        let cmd: UiIn = serde_json::from_str(text)?;
        match cmd {
            UiIn::SetName { name } => self.set_name(name).await,
            UiIn::Send {
                body,
                to,
                group_id,
                reply_to,
            } => self.send_chat(body, to, group_id, reply_to).await,
            UiIn::SendFile {
                file_name,
                data_b64,
                to,
                group_id,
            } => self.send_file(file_name, data_b64, to, group_id).await,
            UiIn::CreateGroup { name, members } => self.create_group(name, members).await,
            UiIn::UpdateGroup { id, name, members } => self.update_group(id, name, members).await,
            UiIn::LeaveGroup { id } => self.leave_group(id).await,
            UiIn::DeleteGroup { id } => self.delete_group(id).await,
            UiIn::RequestFile { file_id, from } => self.request_file(file_id, from).await,
            UiIn::GetConfig => {
                self.publish_config().await;
                Ok(())
            }
            UiIn::SetConfig { patch } => self.set_config(patch).await,
            UiIn::CompleteOnboarding { name } => self.complete_onboarding(name).await,
            UiIn::AddManualPeer { addr } => self.add_manual_peer(addr).await,
            UiIn::RemoveManualPeer { addr } => self.remove_manual_peer(addr).await,
            UiIn::ReconnectPeers => self.reconnect_peers().await,
            UiIn::Resend { id } => self.resend_message(id).await,
            UiIn::ResetNode => self.reset_node().await,
            UiIn::GetInvite => {
                self.publish_invite().await;
                Ok(())
            }
            UiIn::RefreshInvite => self.refresh_invite().await,
            UiIn::GetMeshStats => {
                self.inner.lock().await.last_stats_sig = None;
                self.publish_stats().await;
                Ok(())
            }
            UiIn::GetStorage => {
                self.publish_storage().await;
                Ok(())
            }
            UiIn::DeleteMessages { chat_key } => self.delete_chat_messages(chat_key).await,
            UiIn::DeleteFile { file_id } => self.delete_cached_file(file_id).await,
            UiIn::PurgeFileCache => self.purge_file_cache().await,
            UiIn::DeleteMessage { id } => self.delete_one_message(id).await,
            UiIn::ExportFile { file_id } => self.export_cached_file(file_id).await,
            UiIn::EditMessage { id, body } => self.edit_message(id, body).await,
            UiIn::Forward { id, to, group_id } => self.forward_message(id, to, group_id).await,
            UiIn::DeleteFileOffer { file_id } => self.delete_file_offer(file_id).await,
            UiIn::SetAvatar { data_b64, mime } => self.set_avatar(data_b64, mime).await,
        }
    }
}

fn bind_discovery_socket(port: u16) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let std_sock: std::net::UdpSocket = socket.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

fn parse_discovery(bytes: &[u8]) -> Option<(u16, Option<String>)> {
    let s = std::str::from_utf8(bytes).ok()?;
    let mut parts = s.split('|');
    let magic = parts.next()?;
    if magic.as_bytes() != DISCOVERY_MAGIC {
        return None;
    }
    let _node_id = parts.next()?;
    let _name = parts.next()?;
    let port: u16 = parts.next()?.parse().ok()?;
    let code = parts
        .next()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    Some((port, code))
}

fn short_id(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

fn group_to_info(g: StoredGroup) -> GroupInfo {
    GroupInfo {
        id: g.id,
        name: g.name,
        members: g.members,
        owner: g.owner,
        seq: g.seq,
    }
}

fn sanitize_group_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect()
}

fn is_usable_mesh_ip(addr: IpAddr) -> bool {
    if addr.is_loopback() || addr.is_unspecified() || addr.is_multicast() {
        return false;
    }
    match addr {
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => !(v6.is_unicast_link_local() || v6.to_ipv4_mapped().is_some()),
    }
}

/// Lower is better: prefer private IPv4 LAN over public / v6.
fn mdns_addr_rank(addr: IpAddr) -> u8 {
    match addr {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(v4) if !v4.is_link_local() => 1,
        IpAddr::V6(_) => 2,
        IpAddr::V4(_) => 3,
    }
}

fn addrs_same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    if a.port() != b.port() {
        return false;
    }
    // 0.0.0.0 / :: means "listen on all interfaces" — it is NOT equal to a remote LAN IP.
    // Only treat as self when both are unspecified, both loopback, or IPs match exactly.
    if a.ip().is_unspecified() || b.ip().is_unspecified() {
        return a.ip().is_unspecified() && b.ip().is_unspecified();
    }
    (a.ip().is_loopback() && b.ip().is_loopback()) || a.ip() == b.ip()
}

fn advertise_addr(bind_ip: IpAddr, port: u16) -> String {
    if is_usable_mesh_ip(bind_ip) {
        return SocketAddr::new(bind_ip, port).to_string();
    }
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        let mut addrs: Vec<IpAddr> = ifs
            .iter()
            .filter(|i| !i.is_loopback())
            .map(|i| i.ip())
            .filter(|a| is_usable_mesh_ip(*a))
            .collect();
        addrs.sort_by_key(|a| mdns_addr_rank(*a));
        if let Some(ip) = addrs.first() {
            return SocketAddr::new(*ip, port).to_string();
        }
    }
    format!("{bind_ip}:{port}")
}

fn parse_peer_target(raw: &str) -> Result<(SocketAddr, Option<String>)> {
    let s = raw.trim();
    if looks_like_code(s) {
        let addr = invite_code::decode_addr(s)?;
        return Ok((addr, None));
    }
    if let Some(rest) = s.strip_prefix("mrless://peer/") {
        let bytes = B64.decode(rest.as_bytes()).context("decode invite")?;
        let decoded = String::from_utf8(bytes).context("invite utf8")?;
        let parts: Vec<&str> = decoded.split('|').collect();
        if parts.len() < 3 {
            return Err(anyhow!("invalid invite"));
        }
        let addr: SocketAddr = parts[2]
            .parse()
            .map_err(|_| anyhow!("invalid invite address"))?;
        let token = parts
            .get(3)
            .map(|t| t.to_string())
            .filter(|t| !t.is_empty());
        return Ok((addr, token));
    }
    let addr: SocketAddr = s.parse().map_err(|_| anyhow!("invalid peer addr: {s}"))?;
    Ok((addr, None))
}
