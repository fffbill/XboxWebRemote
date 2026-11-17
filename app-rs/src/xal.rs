use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::signing;
use crate::state::{Flow, UserTok};
use crate::{log, App};

pub const APP_ID: &str = "000000004c20a908";
pub const TITLE_ID: &str = "328178078";
pub const REDIRECT_URI: &str = "ms-xal-000000004c20a908://auth";
pub const SCOPE_MBI: &str = "service::user.auth.xboxlive.com::MBI_SSL";
pub const SCOPE_LPT: &str =
    "service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN";

pub fn now() -> f64 {
    signing::now()
}

pub fn trunc(v: &Value) -> String {
    let s = v.to_string();
    s.chars().take(300).collect()
}

/// 通用 HTTPS JSON 请求 (Python http_json 移植)。返回 (status, 解析后的 JSON/字符串)
pub async fn http_json(
    app: &App,
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
) -> Result<(u16, Value), String> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = app.client.request(m, url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    if let Some(b) = body {
        req = req.body(b);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let val: Value = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    Ok((status, val))
}

/// 带响应头的版本 (SISU authenticate 需要 x-sessionid)
pub async fn http_json_headers(
    app: &App,
    url: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> Result<(u16, Value, std::collections::HashMap<String, String>), String> {
    let mut req = app.client.post(url).body(body);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let mut hdrs = std::collections::HashMap::new();
    for (k, v) in resp.headers() {
        if let Ok(vs) = v.to_str() {
            hdrs.insert(k.as_str().to_ascii_lowercase(), vs.to_string());
        }
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let val: Value = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    Ok((status, val, hdrs))
}

pub fn proof_key(keys: &crate::state::Keys) -> Value {
    json!({"use": "sig", "alg": "ES256", "kty": "EC", "crv": "P-256",
           "x": keys.x, "y": keys.y})
}

pub fn save_tokens(app: &App, p: &crate::state::Persist) {
    let path = app.data_dir.join("tokens.json");
    let tmp = path.with_extension("json.tmp");
    if let Ok(data) = serde_json::to_string_pretty(p) {
        if std::fs::write(&tmp, data).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            }
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub fn load_tokens(app: &App) {
    let path = app.data_dir.join("tokens.json");
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(p) = serde_json::from_str::<crate::state::Persist>(&data) {
            let mut st = app.persist.lock().unwrap();
            *st = p;
            log!("已加载本地令牌 (上次登录保留)");
        }
    }
}

// ---------------------------------------------------------------- 设备令牌

async fn device_token_inner(app: &App, force: bool) -> Result<String, String> {
    {
        let p = app.persist.lock().unwrap();
        if let Some(d) = &p.xaldevice {
            if !force && d.expires_at - 300.0 > now() {
                return Ok(d.token.clone());
            }
        }
    }
    let keys = signing::ensure_keys(&app);
    let payload = json!({
        "Properties": {
            "AuthMethod": "ProofOfPossession",
            "Id": format!("{{{}}}", uuid::Uuid::new_v4()),
            "DeviceType": "Android",
            "SerialNumber": format!("{{{}}}", uuid::Uuid::new_v4()),
            "Version": "15.0",
            "ProofKey": proof_key(&keys),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    let body = payload.to_string();
    let sig = signing::xal_sign(&keys, "/device/authenticate", &body);
    let (status, data) = http_json(
        app,
        "POST",
        "https://device.auth.xboxlive.com/device/authenticate",
        &[
            ("Content-Type", "application/json".into()),
            ("x-xbl-contract-version", "1".into()),
            ("Cache-Control", "no-store, must-revalidate, no-cache".into()),
            ("Signature", sig),
        ],
        Some(body.into_bytes()),
    )
    .await?;
    let token = data.get("Token").and_then(|t| t.as_str()).map(String::from);
    if status != 200 || token.is_none() {
        return Err(format!("设备令牌获取失败: {}", trunc(&data)));
    }
    let token = token.unwrap();
    {
        let mut p = app.persist.lock().unwrap();
        p.xaldevice = Some(crate::state::DeviceTok {
            token: token.clone(),
            expires_at: now() + 36000.0,
        });
    }
    {
        let p = app.persist.lock().unwrap();
        save_tokens(app, &p);
    }
    Ok(token)
}

// ---------------------------------------------------------------- 登录流程

pub async fn begin_login(app: std::sync::Arc<App>) -> Result<String, String> {
    let _g = app.auth_lock.lock().await;
    let dt = device_token_inner(&app, false).await?;
    let verifier = signing::b64url(&rand_bytes(32));
    let challenge = signing::b64url(&Sha256::digest(verifier.as_bytes()).to_vec());
    let state = signing::b64url(&rand_bytes(48));
    let payload = json!({
        "AppId": APP_ID,
        "TitleId": TITLE_ID,
        "RedirectUri": REDIRECT_URI,
        "DeviceToken": dt,
        "Sandbox": "RETAIL",
        "TokenType": "code",
        "Offers": [SCOPE_MBI],
        "Query": {
            "display": "android_phone",
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "state": state,
        },
    });
    let body = payload.to_string();
    let keys = signing::ensure_keys(&app);
    let sig = signing::xal_sign(&keys, "/authenticate", &body);
    let (status, data, hdrs) = http_json_headers(
        &app,
        "https://sisu.xboxlive.com/authenticate",
        &[
            ("Content-Type", "application/json".into()),
            ("x-xbl-contract-version", "1".into()),
            ("Cache-Control", "no-store, must-revalidate, no-cache".into()),
            ("Signature", sig),
        ],
        body.into_bytes(),
    )
    .await?;
    let redirect = data
        .get("MsaOauthRedirect")
        .and_then(|v| v.as_str())
        .map(String::from);
    if status != 200 || redirect.is_none() {
        return Err(format!("SISU 会话创建失败: {}", trunc(&data)));
    }
    let session_id = hdrs.get("x-sessionid").cloned().unwrap_or_default();
    {
        let mut f = app.flow.lock().await;
        *f = Some(Flow {
            verifier,
            state,
            session_id,
            device_token: dt,
        });
    }
    // 保持原始 ms-xal:// 跳转: 由 XboxLoginHelper.app 自动接收授权码
    let base = redirect.unwrap();
    let params = data.get("MsaRequestParameters").cloned().unwrap_or(json!({}));
    let sep = if base.contains('?') { "&" } else { "?" };
    let mut q = form_urlencoded::Serializer::new(String::new());
    if let Some(map) = params.as_object() {
        for (k, v) in map {
            q.append_pair(k, v.as_str().unwrap_or(&v.to_string()));
        }
    }
    Ok(format!("{}{}{}", base, sep, q.finish()))
}

fn rand_bytes(n: usize) -> Vec<u8> {
    // rand 0.10 的 ThreadRng 只实现 TryRngCore (fill_bytes 不可用);
    // uuid v4 本身来自 CSPRNG, 拼接字节作为随机源足够
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        v.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    v.truncate(n);
    v
}

async fn user_access_inner(app: &App) -> Result<String, String> {
    let (access, refresh, expired) = {
        let p = app.persist.lock().unwrap();
        match &p.xaluser {
            Some(u) if u.expires_at - 60.0 > now() && !u.access_token.is_empty() => {
                (Some(u.access_token.clone()), u.refresh_token.clone(), false)
            }
            Some(u) => (None, u.refresh_token.clone(), false),
            None => (None, None, true),
        }
    };
    if let Some(a) = access {
        return Ok(a);
    }
    let refresh = match refresh {
        Some(r) if !r.is_empty() => r,
        _ => return Err("未登录".into()),
    };
    let body = form_urlencoded_body(&[
        ("client_id", APP_ID),
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh),
        ("scope", SCOPE_MBI),
    ]);
    let (status, data) = http_json(
        app,
        "POST",
        "https://login.live.com/oauth20_token.srf",
        &[("Content-Type", "application/x-www-form-urlencoded".into())],
        Some(body.into_bytes()),
    )
    .await?;
    let at = data.get("access_token").and_then(|v| v.as_str()).map(String::from);
    if status != 200 || at.is_none() {
        {
            let mut p = app.persist.lock().unwrap();
            p.xaluser = None;
        }
        {
            let p = app.persist.lock().unwrap();
            save_tokens(app, &p);
        }
        return Err(format!(
            "刷新登录令牌失败, 请重新登录: {}",
            trunc(&data)
        ));
    }
    let at = at.unwrap();
    let new_rt = data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or(refresh);
    let expires_in = data.get("expires_in").and_then(|v| v.as_f64()).unwrap_or(3600.0);
    let tok = UserTok {
        access_token: at,
        refresh_token: Some(new_rt),
        expires_at: now() + expires_in,
    };
    {
        let mut p = app.persist.lock().unwrap();
        p.xaluser = Some(tok.clone());
    }
    {
        let p = app.persist.lock().unwrap();
        save_tokens(app, &p);
    }
    Ok(tok.access_token)
}

pub async fn user_access(app: std::sync::Arc<App>) -> Result<String, String> {
    let _g = app.auth_lock.lock().await;
    user_access_inner(&app).await
}

async fn sisu_authorize_inner(
    app: &App,
    access_token: &str,
    device_token: &str,
    session_id: Option<&str>,
) -> Result<(), String> {
    let keys = signing::ensure_keys(app);
    let mut payload = json!({
        "AccessToken": format!("t={}", access_token),
        "AppId": APP_ID,
        "DeviceToken": device_token,
        "Sandbox": "RETAIL",
        "SiteName": "user.auth.xboxlive.com",
        "UseModernGamertag": true,
        "ProofKey": proof_key(&keys),
    });
    if let Some(sid) = session_id {
        if !sid.is_empty() {
            payload["SessionId"] = json!(sid);
        }
    }
    let body = payload.to_string();
    let sig = signing::xal_sign(&keys, "/authorize", &body);
    let (status, sdata) = http_json(
        app,
        "POST",
        "https://sisu.xboxlive.com/authorize",
        &[
            ("Content-Type", "application/json".into()),
            ("x-xbl-contract-version", "1".into()),
            ("Cache-Control", "no-store, must-revalidate, no-cache".into()),
            ("Signature", sig),
        ],
        Some(body.into_bytes()),
    )
    .await?;
    let has_user = sdata.get("UserToken").is_some();
    if status != 200 || !has_user {
        return Err(format!("SISU 授权失败: {}", trunc(&sdata)));
    }
    {
        let mut p = app.persist.lock().unwrap();
        p.sisu = Some(sdata);
        p.webxsts = None;
    }
    log!("SISU UserToken 已刷新");
    Ok(())
}

pub async fn complete_login(app: std::sync::Arc<App>, url_or_code: &str) -> Result<(), String> {
    let _g = app.auth_lock.lock().await;
    let flow = {
        let mut f = app.flow.lock().await;
        f.take().ok_or("没有进行中的登录, 请先重新发起")?
    };
    let mut code = url_or_code.trim().to_string();
    if code.contains("code=") {
        let parsed = url::Url::parse(&code)
            .map(|u| u.query().unwrap_or("").to_string())
            .unwrap_or_else(|_| code.splitn(2, '?').nth(1).unwrap_or("").to_string());
        let params: std::collections::HashMap<String, String> =
            form_urlencoded::parse(parsed.as_bytes())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        code = params
            .get("code")
            .cloned()
            .ok_or("粘贴的内容里没有找到 code= 参数")?;
    }
    let body = form_urlencoded_body(&[
        ("client_id", APP_ID),
        ("code", &code),
        ("code_verifier", &flow.verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE_MBI),
    ]);
    let (status, data) = http_json(
        &app,
        "POST",
        "https://login.live.com/oauth20_token.srf",
        &[("Content-Type", "application/x-www-form-urlencoded".into())],
        Some(body.into_bytes()),
    )
    .await?;
    let at = data.get("access_token").and_then(|v| v.as_str()).map(String::from);
    if status != 200 || at.is_none() {
        return Err(format!(
            "授权码换取失败(可能已过期, 请重新发起): {}",
            trunc(&data)
        ));
    }
    let at = at.unwrap();
    {
        let mut p = app.persist.lock().unwrap();
        p.xaluser = Some(UserTok {
            access_token: at.clone(),
            refresh_token: data
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(String::from),
            expires_at: now()
                + data.get("expires_in").and_then(|v| v.as_f64()).unwrap_or(3600.0),
        });
    }
    sisu_authorize_inner(&app, &at, &flow.device_token, Some(&flow.session_id)).await?;
    {
        let mut p = app.persist.lock().unwrap();
        p.xhome = None;
        p.webxsts = None;
    }
    {
        let p = app.persist.lock().unwrap();
        save_tokens(&app, &p);
    }
    log!("XAL 登录完成");
    let app2 = app.clone();
    tokio::spawn(async move {
        if let Err(e) = get_xhome_token(app2, false).await {
            log!("预取 xHome 令牌失败(稍后再试): {}", e);
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- XSTS

fn user_token_stale(app: &App) -> bool {
    let p = app.persist.lock().unwrap();
    match &p.sisu {
        None => true,
        Some(s) => match s.pointer("/UserToken/NotAfter").and_then(|v| v.as_str()) {
            Some(na) => {
                // Xbox 时间: 2026-11-17T15:30:00.1234567Z
                let t = na.split('.').next().unwrap_or("");
                let parsed = time::PrimitiveDateTime::parse(
                    t,
                    &time::macros::format_description!(
                        "[year]-[month]-[day]T[hour]:[minute]:[second]"
                    ),
                )
                .ok();
                match parsed {
                    Some(dt) => dt.assume_utc().unix_timestamp() as f64 - 60.0 < now(),
                    None => false, // 解析不了交给失败重试兜底
                }
            }
            None => false,
        },
    }
}

/// XSTS 令牌 (内层, 需在 auth_lock 内调用)。临期主动刷新 + XErr 重试。
async fn xsts_inner(app: &App, relying_party: &str, force: bool) -> Result<Value, String> {
    if force || user_token_stale(app) {
        log!("SISU UserToken 临期/强制刷新, 自动续签...");
        user_access_inner(app).await?;
        let dev = {
            let p = app.persist.lock().unwrap();
            p.xaldevice.as_ref().map(|d| d.token.clone()).unwrap_or_default()
        };
        let at = {
            let p = app.persist.lock().unwrap();
            p.xaluser.as_ref().map(|u| u.access_token.clone()).unwrap_or_default()
        };
        sisu_authorize_inner(app, &at, &dev, None).await?;
    }
    let user_token = {
        let p = app.persist.lock().unwrap();
        p.sisu
            .as_ref()
            .and_then(|s| s.pointer("/UserToken/Token"))
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    let user_token = user_token.ok_or("未完成 XAL 登录")?;

    let post_xsts = |app: &App, user_token: &str| {
        let keys = signing::ensure_keys(&app);
        let payload = json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [user_token],
            },
            "RelyingParty": relying_party,
            "TokenType": "JWT",
        });
        let body = payload.to_string();
        let sig = signing::xal_sign(&keys, "/xsts/authorize", &body);
        (sig, body)
    };

    let (sig, body) = post_xsts(app, &user_token);
    let (status, data) = http_json(
        app,
        "POST",
        "https://xsts.auth.xboxlive.com/xsts/authorize",
        &[
            ("Content-Type", "application/json".into()),
            ("x-xbl-contract-version", "1".into()),
            ("Cache-Control", "no-store, must-revalidate, no-cache".into()),
            ("Signature", sig),
        ],
        Some(body.clone().into_bytes()),
    )
    .await?;
    if status == 200 && data.get("Token").is_some() {
        return Ok(data);
    }
    let xerr = data.get("XErr").and_then(|v| v.as_i64()).unwrap_or(0);
    if (xerr == 2148916238 || xerr == 2148916258) && !force {
        log!("XSTS 报令牌过期 (XErr {}), 刷新 sisu 后重试...", xerr);
        user_access_inner(app).await?;
        let dev = {
            let p = app.persist.lock().unwrap();
            p.xaldevice.as_ref().map(|d| d.token.clone()).unwrap_or_default()
        };
        let at = {
            let p = app.persist.lock().unwrap();
            p.xaluser.as_ref().map(|u| u.access_token.clone()).unwrap_or_default()
        };
        sisu_authorize_inner(app, &at, &dev, None).await?;
        let user_token = {
            let p = app.persist.lock().unwrap();
            p.sisu
                .as_ref()
                .and_then(|s| s.pointer("/UserToken/Token"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default()
        };
        let (sig, body) = post_xsts(app, &user_token);
        let (status, data) = http_json(
            app,
            "POST",
            "https://xsts.auth.xboxlive.com/xsts/authorize",
            &[
                ("Content-Type", "application/json".into()),
                ("x-xbl-contract-version", "1".into()),
                ("Cache-Control", "no-store, must-revalidate, no-cache".into()),
                ("Signature", sig),
            ],
            Some(body.into_bytes()),
        )
        .await?;
        if status == 200 && data.get("Token").is_some() {
            return Ok(data);
        }
    }
    Err(format!("XSTS({}) 失败: {}", relying_party, trunc(&data)))
}

// ---------------------------------------------------------------- 缓存令牌

pub async fn get_xhome_token(app: std::sync::Arc<App>, force: bool) -> Result<(String, String), String> {
    let _g = app.auth_lock.lock().await;
    {
        let p = app.persist.lock().unwrap();
        if let Some(x) = &p.xhome {
            if !force && x.expires_at - 60.0 > now() {
                return Ok((x.gs_token.clone(), x.host.clone()));
            }
        }
    }
    let xsts = xsts_inner(&app, "http://gssv.xboxlive.com/", force).await?;
    let xsts_token = xsts.get("Token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let (status, data) = http_json(
        &app,
        "POST",
        "https://xhome.gssv-play-prod.xboxlive.com/v2/login/user",
        &[
            ("Content-Type", "application/json".into()),
            ("x-gssv-client", "XboxComBrowser".into()),
        ],
        Some(
            json!({"token": xsts_token, "offeringId": "xhome"})
                .to_string()
                .into_bytes(),
        ),
    )
    .await?;
    let gs = data.get("gsToken").and_then(|v| v.as_str()).map(String::from);
    if status != 200 || gs.is_none() {
        return Err(format!("获取 xHome 串流令牌失败: {}", trunc(&data)));
    }
    let mut host = String::new();
    let regions = data.pointer("/offeringSettings/regions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for r in &regions {
        if r.get("isDefault").and_then(|v| v.as_bool()).unwrap_or(false) {
            host = r.get("baseUri").and_then(|v| v.as_str()).unwrap_or("").to_string();
            break;
        }
    }
    if host.is_empty() {
        host = regions
            .first()
            .and_then(|r| r.get("baseUri"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if host.is_empty() {
        return Err("xHome 令牌响应中没有可用区域".into());
    }
    let host = host.trim_start_matches("https://").trim_end_matches('/').to_string();
    let tok = crate::state::XHomeTok {
        gs_token: gs.unwrap(),
        host: host.clone(),
        expires_at: now()
            + data.get("durationInSeconds").and_then(|v| v.as_f64()).unwrap_or(300.0),
    };
    log!("xHome 令牌已就绪, 网关: {}", host);
    {
        let mut p = app.persist.lock().unwrap();
        p.xhome = Some(tok.clone());
    }
    {
        let p = app.persist.lock().unwrap();
        save_tokens(&app, &p);
    }
    Ok((tok.gs_token, tok.host))
}

pub async fn get_web_xsts(app: std::sync::Arc<App>, force: bool) -> Result<(String, String), String> {
    let _g = app.auth_lock.lock().await;
    {
        let p = app.persist.lock().unwrap();
        if let Some(w) = &p.webxsts {
            if !force && w.expires_at - 60.0 > now() {
                return Ok((w.uhs.clone(), w.token.clone()));
            }
        }
    }
    let data = xsts_inner(&app, "http://xboxlive.com", force).await?;
    let token = data.get("Token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let uhs = data
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    {
        let mut p = app.persist.lock().unwrap();
        p.webxsts = Some(crate::state::WebXsts {
            token: token.clone(),
            uhs: uhs.clone(),
            expires_at: now() + 3000.0,
        });
    }
    {
        let p = app.persist.lock().unwrap();
        save_tokens(&app, &p);
    }
    Ok((uhs, token))
}

pub async fn get_lpt(app: std::sync::Arc<App>) -> Result<String, String> {
    {
        let l = app.lpt.lock().unwrap();
        if let Some(t) = &l.0 {
            if now() - l.1 < 90.0 {
                return Ok(t.clone());
            }
        }
    }
    user_access(app.clone()).await?;
    let refresh = {
        let p = app.persist.lock().unwrap();
        p.xaluser.as_ref().and_then(|u| u.refresh_token.clone())
    };
    let refresh = refresh.ok_or("未登录")?;
    let body = form_urlencoded_body(&[
        ("client_id", APP_ID),
        ("scope", SCOPE_LPT),
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh),
    ]);
    let (status, data) = http_json(
        &app,
        "POST",
        "https://login.live.com/oauth20_token.srf",
        &[("Content-Type", "application/x-www-form-urlencoded".into())],
        Some(body.into_bytes()),
    )
    .await?;
    // 该接口的响应字段名是 lpt (不是标准的 access_token)
    let token = data
        .get("lpt")
        .or_else(|| data.get("access_token"))
        .and_then(|v| v.as_str())
        .map(String::from);
    if status != 200 || token.is_none() {
        return Err(format!("获取主机握手令牌(lpt)失败: {}", trunc(&data)));
    }
    let token = token.unwrap();
    {
        let mut l = app.lpt.lock().unwrap();
        l.0 = Some(token.clone());
        l.1 = now();
    }
    Ok(token)
}

pub fn form_urlencoded_body(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, *v)))
        .finish()
}
