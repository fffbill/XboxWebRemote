# 原生渲染重构设计（下一步实施）

> 目标: 移除 wry/WebView，用纯 Rust + Apple 原生框架实现视频渲染、音频输出、手柄输入。
> 现有代码全部保留在 app-rs/，本设计新增 `src/native/` 模块。

## 目标架构

```
tao 窗口 (无 WebView, contentView 直接挂 AVSampleBufferDisplayLayer)
├── nano-rs 引擎 (WebRTC 会话, 信令, 数据通道, 输入协议) ← 已有
│   ├── VideoFrameSink → AVSampleBufferDisplayLayer (VideoToolbox 硬解+渲染)
│   ├── AudioFrameSink → libopus 解码 → cpal 播放
│   └── InputFrame ← GameController 框架直读
├── 服务层 (auth/proxy/power/live) ← 已有
└── 控制面板: 原生 NSMenu + 键盘快捷键 (无 HTML)
```

## 核心组件设计

### 1. 视频解码+渲染 (最关键)

**AVSampleBufferDisplayLayer (推荐, 最简单)**
- nano-rs `VideoFrameSink::push_h264_access_unit(data, pts, keyframe)` 交付 Annex-B 格式 AU
- 转换 Annex-B → AVCC (去起始码, 加 4 字节长度前缀)
- 首次关键帧: 从 SPS+PPS 创建 CMVideoFormatDescription
- 每个 AU → CMBlockBuffer + CMSampleBufferCreateReady → AVSampleBufferDisplayLayer.enqueueSampleBuffer
- AVSampleBufferDisplayLayer 自动调用 VideoToolbox 硬解并渲染
- 关键 API (CoreMedia framework):
  - CMVideoFormatDescriptionCreateFromH264ParameterSets
  - CMBlockBufferCreateWithMemoryBlock + CMBlockBufferReplaceDataBytes
  - CMSampleBufferCreateReady
  - CMTimebaseCreateWithSourceClock + CMTimebaseSetTime/SetRate (主机时钟, 实时)

实现文件: `src/native/video.rs`

### 2. 音频输出

- nano-rs `AudioFrameSink::push_opus_frame(data, pts, sr, ch)` 交付 Opus 帧 (48kHz stereo)
- libopus 解码 (vendor/opus-1.5.2 已 vendored, build.rs 编译): `opus_decode()` → i16 PCM
- cpal 输出流: 环形缓冲 → 回调读取 → 播放
- 缓冲策略: 目标 100ms, 欠载填静音, 超载丢帧

实现文件: `src/native/audio.rs`

### 3. 手柄输入

- objc2-game-controller crate: GCController.controllers() → extendedGamepad profile
- 60Hz 轮询 (或 change handler) → InputFrame { a,b,x,y, dpad, shoulders, thumbs, triggers, axes, menu, nexus }
- 通过 `NanoSessionHandle::send_gamepad_frame(frame)` 发送到引擎数据通道
- Xbox/DualSense/MFi 统一: GameController 框架统一为 extendedGamepad 布局 (位置即映射)

实现文件: `src/native/gamepad.rs`

### 4. 窗口

- tao Window (无 WebView) + NSView 挂 AVSampleBufferDisplayLayer
- NSView 指针: raw-window-handle crate → RawWindowHandle::AppKit(ns_view)
- 窗口 resize: AVSampleBufferDisplayLayer 自动跟随 (setFrame 同步)
- 全屏: NSWindow.toggleFullScreen

### 5. 控制面板 (可选)

- 原生 NSMenu: 连接/断开/画质/退出
- 状态: 窗口标题栏显示 (分辨率/码率/RTT)
- 或透明 overlay (NSView + attributedString)

## 已知坑

| 坑 | 解决 |
|---|---|
| rustls 双提供器歧义 → DTLS panic | main 开头 install_default() |
| WKWebView 隐藏候选 IP (mDNS) | 不依赖浏览器 stats, 用 Rust 引擎的 ICE 候选 |
| objc2 0.6 msg_send 未类型化对象 | 用 raw ffi (objc_msgSend transmute) 或声明 typed extern |
| Retained::from_raw 返回 Option | unwrap (从 msg_send 返回的指针总是有效) |
| CGRect 需要 Encode | 用 objc2-core-foundation crate 的 CGRect (有 Encode) |
| libopus 静态编译 | build.rs + cc crate, 排除 encoder 文件, 解码-only |
| nano-rs 宏 (nano_log 等) | #[macro_export] 在 crate 根, 子模块需 use crate::nano_log |

## 实施步骤

1. 清理: 移除 wry/www 相关代码和依赖
2. 添加 tao 窗口 (无 WebView), 获取 NSView
3. 实现 AVSampleBufferDisplayLayer 渲染 (视频)
4. 实现 libopus 解码 + cpal 播放 (音频)
5. 实现 GameController → InputFrame (手柄)
6. 会话管理: 连接/断开/重连/画质切换
7. 打包: build_app.sh 适配 (cargo build → .app bundle)
8. 验证: 视频显示→手柄输入→音频→关机→重连

## 验证

每个里程碑逐步验证:
1. cargo build 通过
2. 窗口打开, 视频渲染 (1080p60)
3. 手柄输入生效 (游戏内角色移动)
4. 音频同步
5. 断开/重连正常
6. 打包 .app 双击可用
