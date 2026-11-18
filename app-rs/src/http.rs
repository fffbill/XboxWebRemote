use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::live;
use crate::power;
use crate::xal;
use crate::App;

pub const HLS_PORT: u16 = 8978;
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub async fn serve(app: std::sync::Arc<App>) {
    let router = Router::new()
        .route("/", get(static_root))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/start", post(auth_start))
        .route("/api/auth/code", post(auth_code))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/msal", get(api_msal))
        .route("/api/power", post(api_power))
        .route("/api/live/start", post(live_start))
        .route("/api/live/stop", post(live_stop))
        .route("/api/live/status", get(live_status))
        .route("/api/live/chunk", post(live_chunk))
        .route("/live.flv", get(live_flv))
        .route("/v5/{*rest}", any(proxy))
        .route("/v6/{*rest}", any(proxy))
        .route("/v2/{*rest}", any(proxy))
        .route("/{*rest}", get(static_file))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", app.port))
        .await
        .expect("绑定 8977 失败");
    crate::log!("Xbox 网页串流服务已启动: http://127.0.0.1:{}  (Ctrl+C 退出)", app.port);
    axum::serve(listener, router).await.unwrap();
}

pub async fn serve_hls() {
    let router = Router::new().route("/hls/{*name}", get(hls_file)).fallback(hls_not_found);
    match tokio::net::TcpListener::bind(("0.0.0.0", HLS_PORT)).await {
        Ok(l) => {
            crate::log!("局域网 HLS 观看服务: http://{}:{}/hls/live.m3u8  (仅推送时可用)", live::lan_ip(), HLS_PORT);
            let _ = axum::serve(l, router).await;
        }
        Err(e) => crate::log!("HLS 服务启动失败(8978 被占用?): {}", e),
    }
}

// ---------------------------------------------------------------- helpers

fn json_resp(status: StatusCode, v: serde_json::Value) -> Response {
    let body = serde_json::to_string(&v).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .body(axum::body::Body::from(body))
        .unwrap()
}

fn not_found_old() -> Response {
    json_resp(StatusCode::NOT_FOUND, json!({"error": "not found"}))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------- auth

async fn auth_status(State(app): State<std::sync::Arc<App>>) -> Response {
    let mut ok = {
        let p = app.persist.lock().unwrap();
        p.xaluser.is_some() && p.sisu.is_some()
    };
    let mut detail = String::new();
    if ok {
        match xal::get_xhome_token(app.clone(), false).await {
            Ok(_) => {}
            Err(e) => { detail = e; ok = false; }
        }
    }
    json_resp(StatusCode::OK, json!({"authenticated": ok, "detail": detail}))
}

async fn auth_start(State(app): State<std::sync::Arc<App>>) -> Response {
    match xal::begin_login(app.clone()).await {
        Ok(url) => json_resp(StatusCode::OK, json!({"url": url})),
        Err(e) => json_resp(StatusCode::BAD_REQUEST, json!({"error": e})),
    }
}

async fn auth_code(State(app): State<std::sync::Arc<App>>, body: Bytes) -> Response {
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
    if code.is_empty() {
        return json_resp(StatusCode::BAD_REQUEST, json!({"error": "请粘贴登录后跳转的完整地址"}));
    }
    match xal::complete_login(app.clone(), &code).await {
        Ok(_) => json_resp(StatusCode::OK, json!({"status": "ok"})),
        Err(e) => json_resp(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})),
    }
}

async fn auth_logout(State(app): State<std::sync::Arc<App>>) -> Response {
    {
        let mut p = app.persist.lock().unwrap();
        *p = Default::default();
    }
    let path = app.data_dir.join("tokens.json");
    let _ = std::fs::remove_file(path);
    crate::log!("已退出登录, 令牌已删除");
    json_resp(StatusCode::OK, json!({"status": "ok"}))
}

async fn api_msal(State(app): State<std::sync::Arc<App>>) -> Response {
    match xal::get_lpt(app.clone()).await {
        Ok(t) => json_resp(StatusCode::OK, json!({"token": t})),
        Err(e) => json_resp(StatusCode::UNAUTHORIZED, json!({"error": e})),
    }
}

// ---------------------------------------------------------------- power

async fn api_power(State(app): State<std::sync::Arc<App>>, body: Bytes) -> Response {
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let server_id = v.get("serverId").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let action = v.get("action").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if server_id.is_empty() || !["on", "off"].contains(&action.as_str()) {
        return json_resp(StatusCode::BAD_REQUEST, json!({"error": "参数错误, 需要 serverId 和 action(on/off)"}));
    }
    match power::power_command(app.clone(), &server_id, &action).await {
        Ok(r) => json_resp(StatusCode::OK, r),
        Err((code, detail)) => json_resp(
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            detail,
        ),
    }
}

// ---------------------------------------------------------------- live

async fn live_start(State(app): State<std::sync::Arc<App>>) -> Response {
    let r = live::start(app.clone()).await;
    let status = if r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    json_resp(status, r)
}

async fn live_stop(State(app): State<std::sync::Arc<App>>) -> Response {
    json_resp(StatusCode::OK, live::stop(app.clone()).await)
}

async fn live_status(State(app): State<std::sync::Arc<App>>) -> Response {
    json_resp(StatusCode::OK, live::status(app.clone()).await)
}

async fn live_chunk(State(app): State<std::sync::Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let sid = headers.get("x-live-sid").and_then(|h| h.to_str().ok()).map(String::from);
    match live::feed_chunk(app.clone(), sid, body).await {
        Ok(_) => json_resp(StatusCode::OK, json!({"ok": true})),
        Err((code, e)) => json_resp(code, e),
    }
}

async fn live_flv(State(app): State<std::sync::Arc<App>>) -> Response {
    match live::flv_stream(app.clone()).await {
        Ok(r) => r,
        Err((code, e)) => json_resp(code, e),
    }
}

// ---------------------------------------------------------------- proxy

async fn proxy(
    State(app): State<std::sync::Arc<App>>,
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str().to_string()).unwrap_or_else(|| uri.path().to_string());
    let (token, host) = match xal::get_xhome_token(app.clone(), false).await {
        Ok(v) => v,
        Err(e) => {
            return json_resp(StatusCode::UNAUTHORIZED, json!({"error": "尚未登录或令牌失效", "detail": e}));
        }
    };
    let mut req = app.client.request(
        method,
        format!("https://{}{}", host, path_and_query),
    );
    let accept = headers.get("accept").and_then(|h| h.to_str().ok()).unwrap_or("application/json");
    let ctype = headers.get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("application/json");
    req = req.header("Accept", accept);
    req = req.header("Content-Type", ctype);
    req = req.header("Authorization", format!("Bearer {}", token));
    req = req.header("X-Gssv-Client", "XboxComBrowser");
    req = req.header("User-Agent", USER_AGENT);
    if let Some(di) = headers.get("x-ms-device-info") {
        req = req.header("X-MS-Device-Info", di);
    }
    if !body.is_empty() {
        req = req.body(body);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            crate::log!("代理失败: {}", e);
            return json_resp(StatusCode::BAD_GATEWAY, json!({"error": "网关请求失败", "detail": e.to_string()}));
        }
    };
    let status = resp.status();
    let ct = resp.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("application/json").to_string();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return json_resp(StatusCode::BAD_GATEWAY, json!({"error": "网关响应读取失败", "detail": e.to_string()}));
        }
    };
    let mut out = Response::builder().status(status);
    if !bytes.is_empty() {
        out = out.header("Content-Type", ct);
    }
    out.header("Cache-Control", "no-store")
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response())
}

// ---------------------------------------------------------------- static

async fn static_root(State(app): State<std::sync::Arc<App>>) -> Response {
    serve_static(&app, "/")
}

async fn static_file(State(app): State<std::sync::Arc<App>>, Path(path): Path<String>) -> Response {
    serve_static(&app, &path)
}

fn serve_static(app: &App, path: &str) -> Response {
    let rel = if path.is_empty() || path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    let full = std::path::PathBuf::from(&app.www_dir).join(&rel);
    if !full.starts_with(&*app.www_dir) || !full.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }
    let ctype = match full.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    match std::fs::read(&full) {
        Ok(body) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", ctype)
            .header("Cache-Control", "no-cache")
            .body(axum::body::Body::from(body))
            .unwrap(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
    }
}

// ---------------------------------------------------------------- HLS

async fn hls_not_found() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}

async fn hls_file(Path(name): Path<String>) -> Response {
    if name.contains("..") || name.starts_with('/') {
        return (StatusCode::FORBIDDEN, "").into_response();
    }
    if !name.ends_with(".m3u8") && !name.ends_with(".ts") {
        return (StatusCode::FORBIDDEN, "").into_response();
    }
    let full = std::path::Path::new(&live::hls_dir()).join(&name);
    match std::fs::read(&full) {
        Ok(body) => {
            let ct = if name.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", ct)
                .header("Cache-Control", "no-store")
                .body(axum::body::Body::from(body))
                .unwrap()
        }
        Err(_) => (StatusCode::NOT_FOUND, "").into_response(),
    }
}
