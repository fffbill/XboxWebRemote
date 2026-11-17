mod engine;
mod gamepad;
mod http;
mod live;
mod power;
mod session_native;
mod signing;
mod state;
mod win_proto;
mod window;
mod xal;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct App {
    pub client: reqwest::Client,
    pub persist: Mutex<state::Persist>,
    pub auth_lock: tokio::sync::Mutex<()>,
    pub flow: tokio::sync::Mutex<Option<state::Flow>>,
    pub lpt: Mutex<(Option<String>, f64)>,
    pub live: live::Live,
    pub www_dir: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
}

#[macro_export]
macro_rules! log {
    ($fmt:expr $(, $arg:expr)* $(,)?) => {
        println!("[{}] {}", crate::hm_now(), format!($fmt $(, $arg)*))
    };
}

pub fn hm_now() -> String {
    use time::OffsetDateTime;
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

pub fn base_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_www_dir() -> PathBuf {
    if let Ok(env) = std::env::var("XWR_WWW_DIR") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    let exe = exe_dir();
    // .app 包: Contents/MacOS/../Resources/www
    let res = exe.join("../Resources/www");
    if res.is_dir() {
        return res;
    }
    let cwd = base_dir().join("www");
    if cwd.is_dir() {
        return cwd;
    }
    let up = base_dir().join("../www");
    if up.is_dir() {
        return up;
    }
    base_dir().join("www")
}

fn resolve_data_dir() -> PathBuf {
    if let Ok(env) = std::env::var("XWR_DATA_DIR") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    if cfg!(target_os = "macos") {
        let dir = std::env::var("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/XboxWebRemote"))
            .unwrap_or_else(|_| base_dir());
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    base_dir()
}

impl App {
    pub fn new(port: u16) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");
        Self {
            client,
            persist: Mutex::new(state::Persist::default()),
            auth_lock: tokio::sync::Mutex::new(()),
            flow: tokio::sync::Mutex::new(None),
            lpt: Mutex::new((None, 0.0)),
            live: live::Live::new(),
            www_dir: resolve_www_dir(),
            data_dir: resolve_data_dir(),
            port,
        }
    }
}

fn handle_auth_code(url: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ok = rt.block_on(async {
        let client = reqwest::Client::new();
        let body = serde_json::json!({"code": url}).to_string();
        let resp = client
            .post("http://127.0.0.1:8977/api/auth/code")
            .header("Content-Type", "application/json")
            .body(body)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await;
        match resp {
            Ok(r) => {
                let ok = r
                    .status()
                    .is_success()
                    && r.text()
                        .await
                        .map(|t| t.contains("\"ok\""))
                        .unwrap_or(false);
                ok
            }
            Err(_) => false,
        }
    });
    win_proto::notify_result(ok);
}

fn parse_args() -> (u16, bool, Option<String>, Option<String>, u64) {
    let args: Vec<String> = std::env::args().collect();
    let mut port = 8977u16;
    let mut headless = false;
    let mut connect_test: Option<String> = None;
    let mut duration: u64 = 30;
    let mut auth_code = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                if let Some(v) = args.get(i + 1) {
                    port = v.parse().unwrap_or(8977);
                }
                i += 1;
            }
            "--auth-code" => {
                if let Some(v) = args.get(i + 1) {
                    auth_code = Some(v.clone());
                }
                i += 1;
            }
            "--headless" => headless = true,
            "--duration" => {
                if let Some(v) = args.get(i + 1) {
                    duration = v.parse().unwrap_or(30);
                }
                i += 1;
            }
            "--connect-test" => {
                if let Some(v) = args.get(i + 1) {
                    connect_test = Some(v.clone());
                }
                i += 1;
                headless = true;
            }
            _ => {}
        }
        i += 1;
    }
    (port, headless, auth_code, connect_test, duration)
}

fn main() {
    let (port, headless, auth_code, connect_test, duration) = parse_args();

    // rustls 双提供器歧义: webrtc DTLS 握手需要进程级 CryptoProvider (否则 panic)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("crypto provider 安装失败");
    env_logger::init(); // RUST_LOG=debug 可看 webrtc-rs ICE 内部日志

    // 登录回调: Windows 由注册表协议调起第二个实例转交后退出 (macOS 用 Helper.app)
    if let Some(url) = auth_code {
        handle_auth_code(&url);
        std::process::exit(0);
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let app = std::sync::Arc::new(App::new(port));
    xal::load_tokens(&app);

    // 连接测试: 纯 Rust WebRTC 会话连接主机 (无浏览器)
    if let Some(server_id) = connect_test {
        let a = app.clone();
        let result = rt.block_on(async move {
            session_native::run_connect_test(a, &server_id, duration).await
        });
        match result {
            Ok(_) => println!("✅ 连接测试通过"),
            Err(e) => {
                println!("❌ 连接测试失败: {}", e);
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    // 启动服务
    {
        let a = app.clone();
        rt.spawn(async move {
            http::serve(a).await;
        });
        let a = app.clone();
        rt.spawn(async move {
            http::serve_hls().await;
        });
    }

    if headless || !window::available() {
        log!(
            "无窗口模式运行中 — 浏览器打开 http://127.0.0.1:{} 使用",
            port
        );
        rt.block_on(async {
            std::future::pending::<()>().await;
        });
    }

    // 原生窗口 (阻塞主线程直到退出)
    window::run(app.clone(), port);

    std::process::exit(0);
}
