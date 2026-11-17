use serde::{Deserialize, Serialize};

/// 令牌持久化结构 —— 字段名与 Python 版 tokens.json 完全兼容（可互相读写）
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Keys {
    pub pem: String,
    pub x: String,
    pub y: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct UserTok {
    #[serde(rename = "access_token", default)]
    pub access_token: String,
    #[serde(rename = "refresh_token", default)]
    pub refresh_token: Option<String>,
    #[serde(rename = "expires_at", default)]
    pub expires_at: f64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct DeviceTok {
    pub token: String,
    #[serde(rename = "expires_at", default)]
    pub expires_at: f64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct XHomeTok {
    #[serde(rename = "gsToken")]
    pub gs_token: String,
    pub host: String,
    #[serde(rename = "expires_at", default)]
    pub expires_at: f64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct WebXsts {
    pub token: String,
    pub uhs: String,
    #[serde(rename = "expires_at", default)]
    pub expires_at: f64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Persist {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xalkeys: Option<Keys>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xaluser: Option<UserTok>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xaldevice: Option<DeviceTok>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sisu: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhome: Option<XHomeTok>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webxsts: Option<WebXsts>,
}

/// 进行中的 SISU 登录流程
#[derive(Clone)]
pub struct Flow {
    pub verifier: String,
    pub state: String,
    pub session_id: String,
    pub device_token: String,
}
