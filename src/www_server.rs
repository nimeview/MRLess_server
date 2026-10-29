//! Minimal static file server for packaged desktop builds (Windows / portable).
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub async fn serve_www(www_root: PathBuf, port: u16, ws_port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind UI http 127.0.0.1:{port}"))?;
    tracing::info!("UI http://127.0.0.1:{port} → ws://127.0.0.1:{ws_port}");

    loop {
        let (mut sock, _) = listener.accept().await?;
        let root = www_root.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match sock.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let rel = if path == "/" { "/index.html" } else { path };
            let rel = rel.split('?').next().unwrap_or(rel);
            let file = safe_join(&root, rel);
            let (status, mut body, ctype) =
                match file.and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
                    Some((p, bytes)) => ("200 OK", bytes, mime_for(&p)),
                    None => (
                        "404 Not Found",
                        b"not found".to_vec(),
                        "text/plain; charset=utf-8",
                    ),
                };
            if ctype.starts_with("text/html") {
                let inject =
                    format!("<script>window.__MRLESS_WS='ws://127.0.0.1:{ws_port}';</script>");
                if let Ok(html) = String::from_utf8(body.clone()) {
                    if let Some(i) = html.find("<head>") {
                        let mut out = String::new();
                        out.push_str(&html[..i + 6]);
                        out.push_str(&inject);
                        out.push_str(&html[i + 6..]);
                        body = out.into_bytes();
                    }
                }
            }
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-cache\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        });
    }
}

fn safe_join(root: &Path, url_path: &str) -> Option<PathBuf> {
    let trimmed = url_path.trim_start_matches('/');
    if trimmed.contains("..") {
        return None;
    }
    let path = root.join(trimmed);
    let canon_root = root.canonicalize().ok()?;
    let canon = path.canonicalize().ok()?;
    if canon.starts_with(&canon_root) {
        Some(canon)
    } else {
        None
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
