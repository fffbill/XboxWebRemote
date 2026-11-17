// Windows: 注册 ms-xal:// 协议处理器 + 登录结果提示
#[cfg(target_os = "windows")]
pub fn register_protocol() {
    use std::process::Command;
    let python = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cmd = format!("\"{}\" --auth-code \"%1\"", python);
    let keys = [
        r"Software\Classes\ms-xal-000000004c20a908",
        r"Software\Classes\ms-xal-000000004c20a908\DefaultIcon",
        r"Software\Classes\ms-xal-000000004c20a908\shell\open\command",
    ];
    for k in keys {
        let _ = Command::new("reg").args(["add", &format!("HKCU\\{}", k), "/f"]).output();
    }
    let _ = Command::new("reg").args(["add", r"HKCU\Software\Classes\ms-xal-000000004c20a908", "/ve", "/d", "url:Xbox Web Remote Login", "/f"]).output();
    let _ = Command::new("reg").args(["add", r"HKCU\Software\Classes\ms-xal-000000004c20a908\URL Protocol", "/f"]).output();
    let _ = Command::new("reg").args(["add", r"HKCU\Software\Classes\ms-xal-000000004c20a908\shell\open\command", "/ve", "/d", &cmd, "/f"]).output();
}

#[cfg(target_os = "windows")]
pub fn alert(ok: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION};
    let text: Vec<u16> = if ok {
        "登录成功！请回到串流页面\0".encode_utf16().collect()
    } else {
        "处理失败：请确认服务正在运行后重试\0".encode_utf16().collect()
    };
    let title: Vec<u16> = "Xbox 网页串流\0".encode_utf16().collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_ICONINFORMATION);
    }
}

#[cfg(target_os = "windows")]
pub fn register_if_windows() {
    register_protocol();
}

#[cfg(not(target_os = "windows"))]
pub fn register_if_windows() {}

#[cfg(not(target_os = "windows"))]
pub fn notify_result(_ok: bool) {
    // macOS 由 Helper.app 发通知
}
