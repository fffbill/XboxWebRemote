// 手柄桥接 (仅 macOS): GameController 框架直读 → 60Hz InputFrame 注入
// 支持 Xbox / DualSense / MFi 手柄: GameController 把所有标准手柄统一为
// extendedGamepad 布局 (下=A/叉, 右=B/圈, 左=X/方, 上=Y/三角 — 位置即映射)

use tao::event_loop::EventLoopProxy;

use crate::engine::channels::InputFrame;
use crate::AppEvent;

#[cfg(target_os = "macos")]
pub use mac_impl::spawn;

#[cfg(not(target_os = "macos"))]
pub fn spawn(_proxy: EventLoopProxy<AppEvent>) {
    // 非 macOS: 暂无实现 (浏览器模式兜底)
}

#[cfg(target_os = "macos")]
mod mac_impl {
    use super::{build_input_frame, AppEvent};
    use crate::engine::channels::InputFrame;
    use objc2::rc::Retained;
    use objc2_game_controller::{GCController, GCDevice, GCExtendedGamepad};
    use tao::event_loop::EventLoopProxy;

    pub fn spawn(proxy: EventLoopProxy<AppEvent>) {
        std::thread::spawn(move || {
            let mut had_pad = false;
            let mut last_sig = String::new();
            loop {
                // GameController 的 ObjC 消息发送整体标记 unsafe
                unsafe {
                    let controllers = GCController::controllers();
                    let count = controllers.count();
                    let mut found: Option<(String, InputFrame)> = None;

                    for i in 0..count {
                        let ctrl: Retained<GCController> = controllers.objectAtIndex(i);
                        let Some(eg): Option<Retained<GCExtendedGamepad>> = ctrl.extendedGamepad() else {
                            continue;
                        };
                        let name = ctrl.productCategory().to_string();
                        let frame = build_input_frame(&eg);
                        found = Some((name, frame));
                        break;
                    }

                    match found {
                        Some((name, frame)) => {
                            if !had_pad {
                                had_pad = true;
                                let _ = proxy.send_event(AppEvent::Name(name.clone()));
                            }
                            // InputFrame 有 Debug derive — 直接字符串比对
                            let sig = format!("{:?}", frame);
                            if sig != last_sig {
                                last_sig = sig.clone();
                                let _ = proxy.send_event(AppEvent::Input(frame));
                            }
                        }
                        None => {
                            if had_pad {
                                had_pad = false;
                                last_sig.clear();
                                let _ = proxy.send_event(AppEvent::Input(InputFrame::default()));
                                let _ = proxy.send_event(AppEvent::Name(String::new()));
                            }
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
    }
}

/// GameController 状态 → 引擎 InputFrame
pub fn build_input_frame(eg: &objc2_game_controller::GCExtendedGamepad) -> InputFrame {
    // ObjC 属性读取标记 unsafe (objc2 生成绑定)
    let pressed = |b: Option<&objc2_game_controller::GCControllerButtonInput>| -> u8 {
        b.map(|x| unsafe { x.isPressed() as u8 }).unwrap_or(0)
    };
    let axis = |a: Option<&objc2_game_controller::GCControllerAxisInput>| -> f32 {
        a.map(|x| unsafe { x.value() as f32 }).unwrap_or(0.0)
    };
    unsafe { let mut f = InputFrame::default();
    f.a = pressed(Some(&eg.buttonA()));
    f.b = pressed(Some(&eg.buttonB()));
    f.x = pressed(Some(&eg.buttonX()));
    f.y = pressed(Some(&eg.buttonY()));
    let dp = eg.dpad();
    f.dpad_up = pressed(Some(&dp.up()));
    f.dpad_down = pressed(Some(&dp.down()));
    f.dpad_left = pressed(Some(&dp.left()));
    f.dpad_right = pressed(Some(&dp.right()));
    f.left_shoulder = pressed(Some(&eg.leftShoulder()));
    f.right_shoulder = pressed(Some(&eg.rightShoulder()));
    f.left_thumb = pressed(eg.leftThumbstickButton().as_deref());
    f.right_thumb = pressed(eg.rightThumbstickButton().as_deref());
    // LT/RT: GameController 提供的是二值按钮 (模拟量需 TriggerForce 接口, 绑定未导出)
    f.left_trigger = if unsafe { eg.leftTrigger().isPressed() } { 1.0 } else { 0.0 };
    f.right_trigger = if unsafe { eg.rightTrigger().isPressed() } { 1.0 } else { 0.0 };
    f.left_thumb_x_axis = {
        let v = axis(Some(&eg.leftThumbstick().xAxis()));
        if v.abs() < 0.06 { 0.0 } else { v }
    };
    f.left_thumb_y_axis = {
        let v = axis(Some(&eg.leftThumbstick().yAxis()));
        if v.abs() < 0.06 { 0.0 } else { v }
    };
    f.right_thumb_x_axis = {
        let v = axis(Some(&eg.rightThumbstick().xAxis()));
        if v.abs() < 0.06 { 0.0 } else { v }
    };
    f.right_thumb_y_axis = {
        let v = axis(Some(&eg.rightThumbstick().yAxis()));
        if v.abs() < 0.06 { 0.0 } else { v }
    };
    // PS5: Options=Menu, PS=Nexus; Xbox: Menu/View 同义
    f.menu = pressed(eg.buttonOptions().as_deref());
    f.nexus = pressed(eg.buttonHome().as_deref());
    f.view = 0;
    f
    }
}

