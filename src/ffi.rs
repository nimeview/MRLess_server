use crate::config::{AppConfig, CliOverrides};
use crate::identity::Identity;
use crate::node::Node;
use std::ffi::{CStr, CString};
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_ushort};
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::runtime::Runtime;

struct LiveNode {
    rt: Runtime,
    node: Node,
    ws_port: u16,
    mesh_port: u16,
    node_id: String,
}

static LIVE: Mutex<Option<LiveNode>> = Mutex::new(None);

fn c_str<'a>(p: *const c_char) -> anyhow::Result<&'a str> {
    if p.is_null() {
        return Err(anyhow::anyhow!("null pointer"));
    }
    Ok(unsafe { CStr::from_ptr(p) }.to_str()?)
}

/// Start a mesh node in-process. `data_dir` is required; `name` may be null.
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn mrless_start(data_dir: *const c_char, name: *const c_char) -> c_int {
    let dir = match c_str(data_dir) {
        Ok(s) => PathBuf::from(s),
        Err(_) => return -1,
    };
    let display = if name.is_null() {
        None
    } else {
        c_str(name).ok().map(|s| s.to_string())
    };

    let mut guard = match LIVE.lock() {
        Ok(g) => g,
        Err(_) => return -2,
    };
    if guard.is_some() {
        return 0;
    }

    let _ = tracing_subscriber::fmt().try_init();

    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return -3,
    };

    let started = rt.block_on(async {
        let cfg = AppConfig::load_or_create(&dir, &CliOverrides::default())?;
        let ws_port = cfg.ws_port;
        let mesh_port = cfg.mesh_port;
        let identity = Identity::load_or_create(&dir, display)?;
        let node_id = identity.node_id.to_string();
        let node = Node::start(dir, cfg, identity)?;
        Ok::<_, anyhow::Error>((node, ws_port, mesh_port, node_id))
    });

    match started {
        Ok((node, ws_port, mesh_port, node_id)) => {
            *guard = Some(LiveNode {
                rt,
                node,
                ws_port,
                mesh_port,
                node_id,
            });
            0
        }
        Err(e) => {
            tracing::error!("mrless_start: {e:#}");
            -4
        }
    }
}

#[no_mangle]
pub extern "C" fn mrless_stop() {
    if let Ok(mut g) = LIVE.lock() {
        *g = None;
    }
}

#[no_mangle]
pub extern "C" fn mrless_ws_port() -> c_ushort {
    LIVE.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|n| n.ws_port))
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn mrless_mesh_port() -> c_ushort {
    LIVE.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|n| n.mesh_port))
        .unwrap_or(0)
}

/// Connect to `host:port` discovered by the native Bonjour browser.
#[no_mangle]
pub extern "C" fn mrless_connect_peer(host_port: *const c_char) -> c_int {
    let raw = match c_str(host_port) {
        Ok(s) => s.trim(),
        Err(_) => return -1,
    };
    let addr: SocketAddr = match raw.parse() {
        Ok(a) => a,
        Err(_) => return -2,
    };

    let guard = match LIVE.lock() {
        Ok(g) => g,
        Err(_) => return -3,
    };
    let live = match guard.as_ref() {
        Some(l) => l,
        None => return -4,
    };

    let node = live.node.clone();
    live.rt.spawn(async move {
        node.connect_peer(addr, "native").await;
    });
    0
}

/// Re-dial remembered/manual peers and flush the outbox (iOS foreground revive).
#[no_mangle]
pub extern "C" fn mrless_reconnect_peers() -> c_int {
    let guard = match LIVE.lock() {
        Ok(g) => g,
        Err(_) => return -1,
    };
    let live = match guard.as_ref() {
        Some(l) => l,
        None => return -2,
    };
    let node = live.node.clone();
    live.rt.spawn(async move {
        let _ = node.reconnect_peers().await;
    });
    0
}

#[no_mangle]
pub extern "C" fn mrless_node_id() -> *mut c_char {
    let id = LIVE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|n| n.node_id.clone()))
        .unwrap_or_default();
    CString::new(id)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn mrless_version() -> *mut c_char {
    CString::new(env!("CARGO_PKG_VERSION"))
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
/// Frees a string returned by an MRLess FFI function.
///
/// # Safety
///
/// `p` must be a non-null pointer previously returned by an MRLess FFI
/// function that transfers ownership to the caller, and it must be freed only
/// once.
pub unsafe extern "C" fn mrless_string_free(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            drop(CString::from_raw(p));
        }
    }
}
