use serde_json::json;

use crate::xal;
use crate::{log, App};

/// 远程开关机 (xccs.xboxlive.com 远程管理通道)
pub async fn power_command(app: std::sync::Arc<App>, server_id: &str, action: &str) -> Result<serde_json::Value, (u16, serde_json::Value)> {
    let command = if action == "on" { "WakeUp" } else { "TurnOff" };
    let (uhs, token) = xal::get_web_xsts(app.clone(), false).await.map_err(|e| {
        log!("电源命令鉴权失败: {}", e);
        (401u16, json!({"error": e}))
    })?;
    let payload = json!({
        "destination": "Xbox",
        "type": "Power",
        "command": command,
        "sessionId": uuid::Uuid::new_v4().to_string(),
        "sourceId": "com.microsoft.smartglass",
        "parameters": [],
        "linkedXboxId": server_id,
    });
    let auth = format!("XBL3.0 x={};{}", uhs, token);
    let (status, data) = xal::http_json(
        &app,
        "POST",
        "https://xccs.xboxlive.com/commands",
        &[
            ("Authorization", auth),
            ("Accept-Language", "en-US".into()),
            ("skillplatform", "RemoteManagement".into()),
            ("x-xbl-contract-version", "4".into()),
            ("x-xbl-client-name", "XboxApp".into()),
            ("x-xbl-client-type", "UWA".into()),
            ("x-xbl-client-version", "39.39.22001.0".into()),
            ("Content-Type", "application/json".into()),
        ],
        Some(payload.to_string().into_bytes()),
    )
    .await
    .map_err(|e| (502u16, json!({"error": e})))?;
    log!("电源命令 {} {} -> HTTP {}", command, server_id, status);
    if (200..300).contains(&status) {
        Ok(json!({"ok": true, "command": command, "result": data}))
    } else {
        Err((502, json!({"ok": false, "status": status, "detail": data})))
    }
}
