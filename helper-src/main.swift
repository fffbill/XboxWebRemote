import Foundation
import AppKit

// Xbox 网页串流 - 登录回调助手
// 接收 ms-xal-000000004c20a908:// 协议的 URL (经 Apple Event 送达),
// 把完整回调地址 POST 给本地服务, 由其完成 XAL 登录。
// 构建: swiftc -O -o XboxLoginHelper.app/Contents/MacOS/helper main.swift

func notify(_ text: String) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
    p.arguments = ["-e", "display notification \"\(text)\" with title \"Xbox 网页串流\""]
    try? p.run()
}

func forward(url: String) {
    let body = try? JSONSerialization.data(withJSONObject: ["code": url])
    var req = URLRequest(url: URL(string: "http://127.0.0.1:8977/api/auth/code")!)
    req.httpMethod = "POST"
    req.setValue("application/json", forHTTPHeaderField: "Content-Type")
    req.timeoutInterval = 20
    req.httpBody = body
    let sem = DispatchSemaphore(value: 0)
    var ok = false
    URLSession.shared.dataTask(with: req) { data, _, _ in
        if let d = data,
           let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
           (obj["status"] as? String) == "ok" {
            ok = true
        }
        sem.signal()
    }.resume()
    _ = sem.wait(timeout: .now() + 22)
    notify(ok ? "登录成功！请回到串流页面" : "处理失败：服务未运行或已过期，请重试")
}

final class Delegate: NSObject, NSApplicationDelegate {
    var received = false
    func application(_ application: NSApplication, open urls: [URL]) {
        guard let u = urls.first else { return }
        received = true
        forward(url: u.absoluteString)
        exit(0)
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate

// 无 URL 直接启动(双击)时: 提示就绪并 2 秒后退出
DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
    if !delegate.received {
        notify("登录助手已就绪")
        exit(0)
    }
}
app.run()
