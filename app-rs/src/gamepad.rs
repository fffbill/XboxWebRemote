// 手柄桥接 (仅 macOS): GameController 框架直读 → 60Hz 注入页面
// Windows 支持后续用 gilrs 实现 (本文件 cfg 门控)

use tao::event_loop::EventLoopProxy;

#[derive(Debug, Clone)]
pub enum GpEvent {
    State(String),
    Name(String),
    AllZero,
}

#[cfg(target_os = "macos")]
pub use mac_impl::spawn;

#[cfg(not(target_os = "macos"))]
pub fn spawn(_proxy: EventLoopProxy<GpEvent>) {
    // 非 macOS: 暂无实现, 页面会提示「在浏览器打开」兜底
}

#[cfg(target_os = "macos")]
mod mac_impl {
    use tao::event_loop::EventLoopProxy;
    use super::GpEvent;
    use objc2::rc::Retained;
    use objc2_game_controller::{
    GCController, GCControllerAxisInput, GCControllerButtonInput, GCDevice,
    GCExtendedGamepad,
};

    pub fn spawn(proxy: EventLoopProxy<GpEvent>) {
        std::thread::spawn(move || {
            let mut had_pad = false;
            let mut last_state = String::new();
            loop {
                // GameController 的 ObjC 消息发送整体标记 unsafe
                unsafe {
                let controllers = GCController::controllers();
                let count = controllers.count();
                let mut found: Option<(String, String)> = None;

                for i in 0..count {
                    let (ctrl, eg) = unsafe {
                        let ctrl: Retained<GCController> = controllers.objectAtIndex(i);
                        let eg: Option<Retained<GCExtendedGamepad>> = ctrl.extendedGamepad();
                        (ctrl, eg)
                    };
                    let Some(eg) = eg else {
                        continue;
                    };
                    let name = ctrl.productCategory().to_string();
                    found = Some((name, unsafe { render_state(&eg) }));
                    break;
                }

                match found {
                    Some((name, state)) => {
                        if !had_pad {
                            had_pad = true;
                            let _ = proxy.send_event(super::GpEvent::Name(name.clone()));
                        }
                        if state != last_state {
                            last_state = state.clone();
                            let _ = proxy.send_event(super::GpEvent::State(state));
                        }
                    }
                    None => {
                        if had_pad {
                            had_pad = false;
                            last_state.clear();
                            let _ = proxy.send_event(super::GpEvent::Name(String::new()));
                            let _ = proxy.send_event(super::GpEvent::AllZero);
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
                }
            }
        });
    }

    unsafe fn render_state(eg: &GCExtendedGamepad) -> String {
        unsafe fn bf(b: Option<&objc2_game_controller::GCControllerButtonInput>) -> u8 {
            b.map(|x| x.isPressed() as u8).unwrap_or(0)
        }
        unsafe {
        let vf = |v: f64| -> String {
            let v = if v.abs() < 0.06 { 0.0 } else { v };
            format!("{:.4}", v)
        };
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("\"A\":{}", bf(Some(&eg.buttonA()))));
        parts.push(format!("\"B\":{}", bf(Some(&eg.buttonB()))));
        parts.push(format!("\"X\":{}", bf(Some(&eg.buttonX()))));
        parts.push(format!("\"Y\":{}", bf(Some(&eg.buttonY()))));
        let dp = eg.dpad();
        parts.push(format!("\"DU\":{}", bf(Some(&dp.up()))));
        parts.push(format!("\"DD\":{}", bf(Some(&dp.down()))));
        parts.push(format!("\"DL\":{}", bf(Some(&dp.left()))));
        parts.push(format!("\"DR\":{}", bf(Some(&dp.right()))));
        parts.push(format!("\"LB\":{}", bf(Some(&eg.leftShoulder()))));
        parts.push(format!("\"RB\":{}", bf(Some(&eg.rightShoulder()))));
        parts.push(format!(
            "\"LS\":{}",
            bf(eg.leftThumbstickButton().as_deref())
        ));
        parts.push(format!(
            "\"RS\":{}",
            bf(eg.rightThumbstickButton().as_deref())
        ));
        parts.push(format!("\"LT\":{}", vf(eg.leftTrigger().value() as f64)));
        parts.push(format!("\"RT\":{}", vf(eg.rightTrigger().value() as f64)));
        parts.push(format!("\"MN\":{}", bf(eg.buttonOptions().as_deref())));
        parts.push(format!("\"NX\":{}", bf(eg.buttonHome().as_deref())));
        let ls = eg.leftThumbstick();
        let rs = eg.rightThumbstick();
        parts.push(format!("\"LX\":{}", vf(ls.xAxis().value() as f64)));
        parts.push(format!("\"LY\":{}", vf(ls.yAxis().value() as f64)));
        parts.push(format!("\"RX\":{}", vf(rs.xAxis().value() as f64)));
        parts.push(format!("\"RY\":{}", vf(rs.yAxis().value() as f64)));
        parts.push("\"ALLZERO\":0".to_string());
        format!("{{{}}}", parts.join(","))
        }
    }
}
