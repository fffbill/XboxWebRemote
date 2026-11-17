import Foundation
import AppKit
import WebKit
import GameController

// Xbox 网页串流 - macOS 原生 App 启动器
// 原生窗口 (WKWebView) 内嵌串流界面 + 管理内嵌 python 服务子进程。
// 手柄桥接: GameController 框架直读 PS5/Xbox 手柄 → 60Hz 注入页面虚拟手柄
// (WKWebView 不支持 Gamepad API, 见 docs/HANDOFF.md)。
// 菜单保留「在浏览器打开」作为备用路径。

let resPath = Bundle.main.resourcePath!
let dataDir = NSHomeDirectory() + "/Library/Application Support/XboxWebRemote"
let logPath = NSHomeDirectory() + "/Library/Logs/XboxWebRemote.log"
let serverURL = URL(string: "http://127.0.0.1:8977")!

func ensureLogFile() {
    try? FileManager.default.createDirectory(
        atPath: NSHomeDirectory() + "/Library/Logs", withIntermediateDirectories: true)
    if !FileManager.default.fileExists(atPath: logPath) {
        FileManager.default.createFile(atPath: logPath, contents: nil)
    }
}

func logLine(_ s: String) {
    ensureLogFile()
    let line = "\(Date()) \(s)\n"
    if let fh = FileHandle(forWritingAtPath: logPath) {
        fh.seekToEndOfFile()
        fh.write(line.data(using: .utf8)!)
        try? fh.close()
    }
}

func portOpen(_ port: UInt16) -> Bool {
    var addr = sockaddr_in()
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = port.bigEndian
    addr.sin_addr.s_addr = inet_addr("127.0.0.1")
    let fd = socket(AF_INET, SOCK_STREAM, 0)
    defer { close(fd) }
    let r = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
    return r == 0
}

var pythonProc: Process?

final class AppDelegate: NSObject, NSApplicationDelegate, WKUIDelegate, WKNavigationDelegate {
    var webView: WKWebView?
    var mainWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildWindow()
        startGamepadBridge()
    }

    // 停止 python 服务 (先 SIGTERM 宽限 2 秒, 再 SIGKILL)
    func stopService() {
        guard let p = pythonProc, p.isRunning else { return }
        p.terminate()
        let deadline = Date().addingTimeInterval(2)
        while p.isRunning && Date() < deadline { usleep(100000) }
        if p.isRunning { kill(p.processIdentifier, SIGKILL) }
    }

    // ⌘Q / 菜单退出: 显式清理后 exit(0)。
    // 不走 NSApp 拆解流程 —— 退出阶段的 autorelease pool 排空会释放
    // WKWebView 遗留对象导致 SIGSEGV (见崩溃报告), exit(0) 绕开该路径,
    // 也保证 python 一定被杀掉 (不会留孤儿进程)。
    @objc func quitApp() {
        webView?.stopLoading()
        webView?.removeFromSuperview()
        stopService()
        logLine("服务已停止, 正常退出")
        exit(0)
    }

    func applicationWillTerminate(_ notification: Notification) {
        // 系统发起的退出 (osascript/其他 App) 也走这里; 直接 exit(0)
        // 跳过 NSApp 拆解, 避免同一处 SIGSEGV。
        stopService()
        logLine("服务已停止, 正常退出")
        exit(0)
    }

    // 再次点击 Dock 图标: 聚焦主窗口
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        mainWindow?.makeKeyAndOrderFront(nil)
        return true
    }

    // 页面加载完成: 标记"运行在 App 内", 供网页提示手柄兼容性
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView.evaluateJavaScript("window.XWR_IN_APP = true")
    }

    // ---------- 主窗口 ----------
    func buildWindow() {
        let win = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800),
                           styleMask: [.titled, .closable, .miniaturizable, .resizable],
                           backing: .buffered, defer: false)
        win.title = "Xbox 网页串流"
        win.minSize = NSSize(width: 800, height: 500)
        win.center()

        let config = WKWebViewConfiguration()
        // 串流音频需要自动播放
        config.mediaTypesRequiringUserActionForPlayback = []

        // 尝试开启 WKWebView 的 Gamepad API (SPI; 不支持该键时安全跳过)
        let prefs = config.preferences
        if prefs.responds(to: Selector(("setGamepadEnabled:"))) {
            prefs.setValue(true, forKey: "gamepadEnabled")
            logLine("已尝试启用 WKWebView gamepad 支持")
        }

        let wv = WKWebView(frame: NSRect(x: 0, y: 0, width: 1280, height: 800), configuration: config)
        wv.autoresizingMask = [NSView.AutoresizingMask.width, NSView.AutoresizingMask.height]
        wv.uiDelegate = self
        wv.navigationDelegate = self
        // UA 追加标记, 网页据此识别 App 环境
        wv.customUserAgent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15 XboxWebRemoteApp"
        win.contentView = wv
        webView = wv
        mainWindow = win
        win.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)

        // 服务未就绪时先轮询再加载 (等待期间显示提示页)
        if portOpen(8977) {
            wv.load(URLRequest(url: serverURL))
        } else {
            wv.loadHTMLString(waitingHTML(), baseURL: nil)
            DispatchQueue.global().async {
                for _ in 0..<40 {
                    if portOpen(8977) {
                        DispatchQueue.main.async {
                            self.webView?.load(URLRequest(url: serverURL))
                        }
                        return
                    }
                    usleep(500000)
                }
                DispatchQueue.main.async {
                    self.webView?.loadHTMLString(timeoutHTML(), baseURL: nil)
                }
            }
        }
    }

    // ---------- 页面 window.open (直播模式独立窗口) ----------
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        let width = windowFeatures.width?.intValue ?? 1280
        let height = windowFeatures.height?.intValue ?? 720
        let win = NSWindow(contentRect: NSRect(x: 60, y: 60, width: width, height: height),
                           styleMask: [.titled, .closable, .resizable],
                           backing: .buffered, defer: false)
        win.title = "Xbox 直播窗口"
        let wv = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height),
                           configuration: configuration)
        wv.autoresizingMask = [NSView.AutoresizingMask.width, NSView.AutoresizingMask.height]
        win.contentView = wv
        win.makeKeyAndOrderFront(nil)
        // 返回后 WebKit 会自动 load navigationAction.request
        return wv
    }

    // ---------- 菜单动作 ----------
    @objc func openInBrowser() {
        NSWorkspace.shared.open(serverURL)
    }

    // ---------- 原生手柄桥接 (GameController → 页面虚拟手柄) ----------
    func startGamepadBridge() {
        NotificationCenter.default.addObserver(
            forName: .GCControllerDidConnect, object: nil, queue: .main) { _ in
            logLine("手柄已连接 (GameController)")
            self.pushNativeGpName()
        }
        NotificationCenter.default.addObserver(
            forName: .GCControllerDidDisconnect, object: nil, queue: .main) { _ in
            let rest = GCController.controllers().contains { $0.extendedGamepad != nil }
            if !rest {
                logLine("手柄已断开, 发送全零状态")
                self.webView?.evaluateJavaScript("window.__nativeGpState = {\"ALLZERO\":1,\"A\":0,\"B\":0,\"X\":0,\"Y\":0,\"DU\":0,\"DD\":0,\"DL\":0,\"DR\":0,\"LB\":0,\"RB\":0,\"LS\":0,\"RS\":0,\"LT\":0,\"RT\":0,\"MN\":0,\"VW\":0,\"NX\":0,\"LX\":0,\"LY\":0,\"RX\":0,\"RY\":0}")
            }
            self.pushNativeGpName()
        }
        // 60Hz 读取并注入页面
        let timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { _ in
            self.pollNativeGamepad()
        }
        RunLoop.main.add(timer, forMode: .common)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.pushNativeGpName() }
    }

    private func firstExtendedController() -> GCController? {
        GCController.controllers().first { $0.extendedGamepad != nil }
    }

    private func pushNativeGpName() {
        let name = firstExtendedController()?.productCategory ?? ""
        webView?.evaluateJavaScript("window.__nativeGpName = \(jsonStr(name)); void 0;")
        logLine("原生手柄: \(name.isEmpty ? "(无)" : name)")
    }

    private func pollNativeGamepad() {
        guard let wv = webView else { return }
        guard let ctrl = firstExtendedController(), let g = ctrl.extendedGamepad else { return }

        func bf(_ e: GCControllerButtonInput?) -> Float { (e?.isPressed ?? false) ? 1 : 0 }
        func av(_ e: GCControllerAxisInput?) -> Float { e?.value ?? 0 }
        func dz(_ v: Float) -> Float { abs(v) < 0.06 ? 0 : v }

        let lx = dz(av(g.leftThumbstick.xAxis)), ly = dz(av(g.leftThumbstick.yAxis))
        let rx = dz(av(g.rightThumbstick.xAxis)), ry = dz(av(g.rightThumbstick.yAxis))
        let lt = g.leftTrigger.value
        let rt = g.rightTrigger.value

        var parts: [String] = []
        parts.append("\"A\":\(bf(g.buttonA))")
        parts.append("\"B\":\(bf(g.buttonB))")
        parts.append("\"X\":\(bf(g.buttonX))")
        parts.append("\"Y\":\(bf(g.buttonY))")
        parts.append("\"DU\":\(bf(g.dpad.up))")
        parts.append("\"DD\":\(bf(g.dpad.down))")
        parts.append("\"DL\":\(bf(g.dpad.left))")
        parts.append("\"DR\":\(bf(g.dpad.right))")
        parts.append("\"LB\":\(bf(g.leftShoulder))")
        parts.append("\"RB\":\(bf(g.rightShoulder))")
        parts.append("\"LS\":\(bf(g.leftThumbstickButton))")
        parts.append("\"RS\":\(bf(g.rightThumbstickButton))")
        parts.append("\"LT\":\(lt)")
        parts.append("\"RT\":\(rt)")
        parts.append("\"MN\":\(bf(g.buttonOptions))")
        parts.append("\"VW\":0")
        parts.append("\"NX\":\(bf(g.buttonHome))")
        parts.append("\"LX\":\(lx)")
        parts.append("\"LY\":\(ly)")
        parts.append("\"RX\":\(rx)")
        parts.append("\"RY\":\(ry)")
        parts.append("\"ALLZERO\":0")
        let state = "{" + parts.joined(separator: ",") + "}"
        wv.evaluateJavaScript("window.__nativeGpState = \(state)")
    }

    private func jsonStr(_ s: String) -> String {
        let data = try? JSONSerialization.data(withJSONObject: [s])
        return data.flatMap { String(data: $0, encoding: .utf8) } ?? "\"\""
    }
}

func waitingHTML() -> String {
    return "<body style='background:#0e1116;color:#8b97a8;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;font-family:sans-serif'><div>正在启动本地服务…</div></body>"
}

func timeoutHTML() -> String {
    return "<body style='background:#0e1116;color:#e05a5a;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;font-family:sans-serif'><div>服务启动超时，日志见 ~/Library/Logs/XboxWebRemote.log</div></body>"
}

// 基本菜单栏
let mainMenu = NSMenu()
let appMenuItem = NSMenuItem()
mainMenu.addItem(appMenuItem)
let appMenu = NSMenu()
let browserItem = NSMenuItem(title: "在浏览器打开", action: #selector(AppDelegate.openInBrowser), keyEquivalent: "b")
appMenu.addItem(browserItem)
appMenu.addItem(.separator())
let quitItem = NSMenuItem(title: "退出 Xbox网页串流", action: #selector(AppDelegate.quitApp), keyEquivalent: "q")
appMenu.addItem(quitItem)
appMenuItem.submenu = appMenu
app.mainMenu = mainMenu

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)

// ---------- 已有服务在跑? 直接复用 (不接管别人的进程) ----------
let serviceRunning = portOpen(8977)

// ---------- 数据目录 / 日志 ----------
try? FileManager.default.createDirectory(atPath: dataDir, withIntermediateDirectories: true)
ensureLogFile()

// ---------- 注册内嵌的登录回调助手 (幂等) ----------
let helperPath = resPath + "/XboxLoginHelper.app"
let lsreg = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if FileManager.default.fileExists(atPath: helperPath) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: lsreg)
    p.arguments = ["-f", helperPath]
    try? p.run()
    p.waitUntilExit()
}

// ---------- 拉起 python 服务 ----------
if !serviceRunning {
    let py = Process()
    py.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
    py.arguments = [resPath + "/server.py"]
    py.currentDirectoryURL = URL(fileURLWithPath: resPath)
    py.environment = ["XWR_DATA_DIR": dataDir,
                      "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                      "HOME": NSHomeDirectory()]
    if let fh = FileHandle(forWritingAtPath: logPath) {
        fh.seekToEndOfFile()
        py.standardOutput = fh
        py.standardError = fh
    }
    pythonProc = py
    do {
        try py.run()
        logLine("服务已启动 (python3 server.py, pid=\(py.processIdentifier))")
    } catch {
        logLine("服务启动失败: \(error)")
    }
} else {
    logLine("8977 已有服务运行, 直接复用")
}

app.run()
