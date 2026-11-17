// 原生窗口管理: 页面 + 原生会话 + 手柄注入
// AppEvent 在 main.rs 定义

use std::sync::{Arc, Mutex};

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::engine::backend::NanoSessionHandle;
use crate::engine::channels::InputFrame;
use crate::{App, AppEvent};

struct SessionState {
    handle: Mutex<Option<NanoSessionHandle>>,
    webview: Mutex<Option<Arc<wry::WebView>>>,
}

pub fn available() -> bool {
    true
}

pub fn run(app: Arc<App>, port: u16) {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // 注册内嵌的登录回调助手 (幂等, macOS)
    #[cfg(target_os = "macos")]
    {
        if let Some(res) = std::env::current_exe().ok().and_then(|p| {
            p.parent()
                .map(|d| d.join("../Resources/XboxLoginHelper.app"))
        }) {
            if res.is_dir() {
                let _ = std::process::Command::new(
                    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
                )
                .arg("-f")
                .arg(&res)
                .output();
            }
        }
    }

    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Xbox 网页串流")
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(800.0, 500.0))
        .build(&event_loop)
        .expect("window");

    let state = Arc::new(SessionState {
        handle: Mutex::new(None),
        webview: Mutex::new(None),
    });

    // IPC: 页面 → 原生 (启动串流)
    let state_ipc = state.clone();
    let proxy_ipc = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_url(format!("http://127.0.0.1:{}", port))
        .with_initialization_script("window.XWR_IN_APP = true;")
        .with_autoplay(true)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let body = req.body().clone();
            let p = proxy_ipc.clone();
            std::thread::spawn(move || {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if v.get("cmd").and_then(|c| c.as_str()) == Some("start-stream") {
                        let server_id = v.get("serverId").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        let height = v.get("height").and_then(|h| h.as_u64()).unwrap_or(1080) as u32;
                        let _ = p.send_event(AppEvent::StartSession { server_id, height });
                    }
                }
            });
        })
        .build(&window)
        .expect("webview");
    {
        let mut slot = state.webview.lock().unwrap();
        *slot = Some(Arc::new(webview));
    }

    crate::log!("原生窗口已打开: http://127.0.0.1:{}", port);

    // 会话句柄 (事件循环持有)
    let mut session: Option<NanoSessionHandle> = None;

    event_loop.run(move |event, _event_loop, control_flow| {
        *control_flow = ControlFlow::Poll;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(AppEvent::Input(_frame)) => {
                // 手柄帧注入引擎 (待实现 InputFrame 管线)
            }
            Event::UserEvent(AppEvent::StartSession { server_id, height }) => {
                let app2 = app.clone();
                let proxy2 = proxy.clone();
                let h = height as u32;
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt.block_on(async move {
                        crate::session_native::start_native_session(app2, None, None, None, h).await
                    });
                    match result {
                        Ok(_) => crate::log!("会话已启动 ({}p)", height),
                        Err(e) => crate::log!("会话启动失败: {}", e),
                    }
                });
            }
            Event::UserEvent(AppEvent::SessionStatus { stage, message, terminal }) => {
                // 页面状态同步
                if let Some(wv) = &*state.webview.lock().unwrap() {
                    let safe_stage = stage.replace('\'', "");
                    let js = format!(
                        "window.xwrNativeStatus && window.xwrNativeStatus('{}', {}, {})",
                        safe_stage,
                        serde_json::json!(message).to_string(),
                        terminal
                    );
                    let _ = wv.evaluate_script(&js);
                }
                if terminal {
                    if let Some(h) = &mut session {
                        let _ = h.stop();
                    }
                    session = None;
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if let Some(h) = &mut session {
                    let _ = h.stop();
                }
                session = None;
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
