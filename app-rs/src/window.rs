use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::gamepad::{GpEvent, spawn as gp_spawn};

pub fn available() -> bool {
    true // tao/wry 全平台可用 (Windows 需 WebView2 runtime)
}

pub fn run(app: std::sync::Arc<crate::App>, port: u16) {
    let event_loop = EventLoopBuilder::<GpEvent>::with_user_event().build();

    // 注册内嵌的登录回调助手 (幂等, macOS)
    #[cfg(target_os = "macos")]
    {
        if let Some(res) = std::env::current_exe().ok().and_then(|p| {
            p.parent()
                .map(|d| d.join("../Resources/XboxLoginHelper.app"))
        }) {
            if res.is_dir() {
                let _ = std::process::Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
                    .arg("-f")
                    .arg(&res)
                    .output();
            }
        }
    }

    let proxy: EventLoopProxy<GpEvent> = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Xbox 网页串流")
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(800.0, 500.0))
        .build(&event_loop)
        .expect("window");

    let webview = WebViewBuilder::new()
        .with_url(format!("http://127.0.0.1:{}", port))
        .with_initialization_script("window.XWR_IN_APP = true;")
        .with_autoplay(true)
        .build(&window)
        .expect("webview");
    let _ = webview;

    // 手柄桥接线程 (60Hz 注入)
    gp_spawn(proxy.clone());

    crate::log!("原生窗口已打开: http://127.0.0.1:{}", port);

    event_loop.run(move |event, _event_loop, control_flow| {
        *control_flow = ControlFlow::Poll;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(GpEvent::State(s)) => {
                let _ = webview.evaluate_script(&format!("window.__nativeGpState = {}", s));
            }
            Event::UserEvent(GpEvent::Name(n)) => {
                let quoted = serde_json::json!(n).to_string();
                let _ = webview.evaluate_script(&format!("window.__nativeGpName = {}", quoted));
            }
            Event::UserEvent(GpEvent::AllZero) => {
                let _ = webview.evaluate_script(
                    "window.__nativeGpState = {\"ALLZERO\":1,\"A\":0,\"B\":0,\"X\":0,\"Y\":0,\"DU\":0,\"DD\":0,\"DL\":0,\"DR\":0,\"LB\":0,\"RB\":0,\"LS\":0,\"RS\":0,\"LT\":0,\"RT\":0,\"MN\":0,\"VW\":0,\"NX\":0,\"LX\":0,\"LY\":0,\"RX\":0,\"RY\":0}",
                );
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                crate::log!("窗口关闭, 退出");
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
