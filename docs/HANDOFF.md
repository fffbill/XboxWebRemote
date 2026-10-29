# 交接文档 (HANDOFF)

> 本文档面向未来接手本项目的任何人（或 AI agent）。
> 记录了项目的全部来龙去脉、协议知识、踩坑史与运行环境。
> 最后更新: 2026-10-30

---

## 1. 项目起源与目标

用户（macOS, 广东移动宽带, 有 Xbox Series X 主机, Game Pass Ultimate 会员）原先用
[Geocld/XStreamingDesktop](https://github.com/Geocld/XStreamingDesktop)（Mac 桌面版）串流，
但该版本握手经境外服务器、延迟高且几秒断连；手机版
[XStreaming](https://github.com/Geocld/XStreaming) 却可以局域网/IPv6 直连。

因此构建了本工具：**浏览器内 Xbox Remote Play**。核心价值 = 媒体流 P2P 直连
（微软只做信令），并逐步扩展出：1440p、远程开关机、直播输出（FLV/HLS）、跨平台。

**用户已明确表示满意**（"我对这个脚本很满意"）。

## 2. 交付物与运行环境

| 位置 | 说明 |
|---|---|
| `~/Desktop/Xbox网页串流/` | **运行版**（用户日常双击使用），含 tokens.json 与 bin/ffmpeg |
| 本仓库 (`~/.../XboxWebRemote/`) | **发布版**源码，git 推送至 github.com/xhlbill/XboxWebRemote |
| 运行方式 | 双击 `双击启动.command`，或 `python3 server.py`；服务监听 127.0.0.1:8977，HLS 监听 0.0.0.0:8978 |
| 推送凭据 | macOS 钥匙串：`security find-generic-password -a xhlbill -s XboxWebRemote-git -w`（GitHub 设备码流获得的 gho_ token, scope=repo；用户可在 GitHub Settings→Applications→GitHub CLI 撤销） |
| 网络 | 终端访问 GitHub 需走系统代理 `http://127.0.0.1:7890`（浏览器走代理而终端默认不走，curl/git 需显式设置） |

⚠️ **tokens.json 含微软账号凭据，绝不入库**（.gitignore 已排除）。

## 3. 架构与数据流

```
浏览器 (www/index.html, WebRTC 播放/输入, 硬解)
   │  ① 信令/登录/开关机 = 小 HTTP 请求 → server.py (127.0.0.1:8977)
   │  ② 媒体流 = P2P 直连主机 (局域网 host / IPv6 / Teredo), 不经过任何服务器
   ▼
server.py:
   - XAL/SISU 登录链 → xHome gsToken → 代理 /v5 /v6 (串流信令)
   - xccs.xboxlive.com → 远程开关机
   - 直播源: 浏览器分片上传(webm) → ffmpeg → FLV(:8977/live.flv) + HLS(8978)
```

解码**只在观看端浏览器**；路由器/服务器不做任何媒体处理。

## 4. 文件清单

| 文件 | 职责 |
|---|---|
| `server.py` | 全部后端（纯标准库 + 可选 cryptography）：托管、XAL 登录、代理、电源、直播源。约 1000 行 |
| `www/index.html` | 单页 App：登录(XAL)、主机列表、串流、画质、统计、直播模式、直播源 |
| `www/live.html` | 直播模式独立窗口（画面+声音，供窗口捕获） |
| `www/lib/xCloudPlayer.min.js` | 播放器内核（xbox-xcloud-player npm 构建产物；**全局变量挂在 `.default` 下**） |
| `www/assets/` | Xbox 标志 + 主机产品图（来自 XStreaming） |
| `XboxLoginHelper.app` + `helper-src/main.swift` | macOS 的 ms-xal:// 协议回调助手（Swift, NSApplicationDelegate open urls → POST /api/auth/code）。构建：`swiftc -O` |
| `双击启动.command` / `启动串流-windows.bat` | 启动器（Win 版会 pip install cryptography + 注册协议） |

## 5. 协议速查（核心资产）

### 5.1 鉴权链（XAL/SISU —— Xbox 手机 App 同款）

```
AppId 000000004c20a908, TitleId 328178078 (xal-node 同款)
本地 P-256 密钥 → ProofOfPossession 签名 (ES256, P1363, 见 xal_sign())
① device.auth.xboxlive.com/device/authenticate        → 设备令牌
② sisu.xboxlive.com/authenticate (响应头 x-sessionid)  → MSA 登录 URL
③ 用户浏览器登录 → 跳转 ms-xal-000000004c20a908://auth?code=...（由 Helper 接收）
④ login.live.com/oauth20_token.srf (client_id=AppId, scope=service::user.auth.xboxlive.com::MBI_SSL)
   注意: client_id 用 MSA AppId(无连字符), 不是 OAuth client
⑤ sisu.xboxlive.com/authorize (AccessToken='t='+v1token, SessionId) → UserToken
⑥ XSTS: xsts.auth.xboxlive.com/xsts/authorize
   Properties: {SandboxId:'RETAIL', UserTokens:[sisu.UserToken.Token]}   ← 只传这个, 无 DeviceToken/TitleToken
   不同 RelyingParty: http://gssv.xboxlive.com/ (串流) / http://xboxlive.com (xccs 开关机)
⑦ lpt (主机握手令牌, 缺失=主机5分钟踢会话):
   login.live.com/oauth20_token.srf, scope=service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN
   响应字段名是 **lpt** 而非 access_token
   只能用 XAL 系 v1 令牌; v2 设备码令牌(任何写法)必返回 server_error
```

签名算法（`xal_sign()`）：
```
buf = int32(1) + 0x00 + FILETIME(8B, win epoch) + 0x00 + "POST" 0x00 + path 0x00 + authz("") 0x00 + payload 0x00
sig = ECDSA-P256-SHA256(buf), DER→r||s(64B)
Signature头 = base64( int32(1) + FILETIME + sig )
```

### 5.2 串流会话（xHome 网关, host 来自 /v2/login/user 的 offeringSettings.regions）

| 操作 | 请求 |
|---|---|
| 建会话 | `POST {host}/v5/sessions/home/play`（body.settings.osName 决定画质档：android=720p/windows=1080p/**tizen=1440p**）|
| 状态轮询 | `GET .../state`（Provisioning→Provisioned→ReadyToConnect）|
| SDP/ICE | `POST .../sdp`、`GET .../sdp`、`POST .../ice`、`GET .../ice` |
| **lpt 握手** | `POST {sessionPath}/connect` body `{userToken: lpt}`（202；**必须在 ReadyToConnect 后发**，库的 waitForState 停轮后要自己续看）|
| 保活 | `POST .../keepalive` 每 30s |
| 清理 | `GET /v5/sessions/home/active` + `DELETE /v5/sessions/home/{id}` |
| 主机列表 | `GET /v6/servers/home?mr=50`（powerState: On/Off/ConnectedStandby）|

**分辨率还需在 message 通道（messageV1）HandshakeAck 后发**
`/streaming/characteristics/dimensionschanged`（preferredWidth/Height=2560×1440），只改 osName 不够。
库的 message.sendConfig() 写死 1080p → 前端 monkey-patch 追加。

### 5.3 远程开关机（xccs.xboxlive.com，与串流网关是两个服务！）

```
POST https://xccs.xboxlive.com/commands
Authorization: XBL3.0 x={uhs};{XSTS(RP=http://xboxlive.com)}
skillplatform: RemoteManagement, x-xbl-contract-version: 4, x-xbl-client-name: XboxApp
body: {destination:'Xbox', type:'Power', command:'WakeUp'|'TurnOff',
       sessionId:uuid4, sourceId:'com.microsoft.smartglass', parameters:[], linkedXboxId:serverId}
```
xhome 网关上**没有**电源端点（/power 全 404）。待机唤醒也可由建会话自动触发。

### 5.4 直播源（FLV/HLS）

```
浏览器 MediaRecorder('video/webm;codecs="h264,opus"', videoBitsPerSecond:8M) 1s 分片
  → POST /api/live/chunk (X-Live-Sid 会话头, Promise 链串行上传!)
  → server.py 喂 ffmpeg stdin
ffmpeg: -f matroska -i pipe:0
        输出1: -c:v h264_videotoolbox(或 nvenc/qsv/amf/x264) -b:v 8M -f flv pipe:1  (FLV 广播)
        输出2: -c:v copy -f hls ... (HLS 分片, 8978 端口只读服务)
拉流: http://127.0.0.1:8977/live.flv (2-4s延迟) / http://LAN_IP:8978/hls/live.m3u8 (10-20s)
```

关键实现点：**WebM 分片必须按序上传**（Promise 链）；FLV tag 级解析维护"配置段+最近关键帧"接入缓冲（新客户端从这接入，prev-size 字段要改写）；新客户端先等关键帧再订阅（顺序反了流会坏）。

## 6. 踩坑史（防止回归, 按发现顺序）

1. **库的坑**：webpack 产物挂在 `xCloudPlayer.default` 下；`startStream()` 成功返回 Stream 对象（用 `getSessionId()`，无公开 .sessionId）且 4xx 时可能正常 resolve；`Channel.send()` 对未开通道**静默丢弃**（手柄必须在 control datachannel open 后 attach）；`waitForState('Provisioned')` 达成后停止轮询（onReadyToConnect 永远不触发，要自己续看）
2. **会话残留**：失败/退出要 DELETE active 会话，否则后续建会话冲突
3. **lpt**：v2 设备码令牌换不了（server_error；X-Forwarded-For 伪装无效；smartglass client 在 v2 不存在）→ 必须 XAL。缺失 = 5 分钟掉线（保活 200 也踢）
4. **MSA 校验**：redirect_uri 不能改（localhost 被拒、oauth20_desktop.srf 被剥码 removed=true），码只走注册的 ms-xal:// 私有协议
5. **macOS 弹窗**：新系统弹窗经 Apple Event 传 URL，shell 脚本 argv 恒空 → 必须 Swift 原生（NSApplicationDelegate open urls）；codesign 前 `xattr -cr`，bundle 根目录不能有杂物
6. **sisu/authorize 响应**只有 TitleToken/UserToken/AuthorizationToken/WebPage
7. **FLV 广播**：分片乱序上传会损坏流（Promise 链串行）；拼接处 prev-size 要改写；先等关键帧再订阅
8. **ffmpeg 多输出**：输出选项只作用于紧邻输出，两路各自声明编码，否则 HLS 默认 libx264 软编吃满 CPU
9. **性能**：MediaRecorder 码率不可控（VBR 冲 20-30Mbps）→ 8Mbps 限制靠 ffmpeg h264_videotoolbox/nvenc 转码 + hwaccel 硬解（ffmpeg CPU 89%→10%）
10. **Chrome macOS 不识别 USB 接的 Xbox 手柄**：Chromium 已知 bug (issues.chromium.org/400455006)，蓝牙正常。页面有 gpPill 状态指示
11. **Xbox 1440p 需主机系统更新**（2026-09 起）；主机显示分辨率须设 1440p/4K；4K Remote Play 官方未开放（协议上申报 3840×2160 主机不理）
12. **主机节能关机模式远程唤不醒**（即时启动可以）；空闲断连是用户主机的节电设置——**用户明确说不用管空闲掉线，只需游玩时不断**

## 7. 验证状态

✅ 已实测（用户真实主机）：IPv6/局域网直连、1440p60@8Mbps、开关机双向、手柄（蓝牙）、直播模式、FLV/HLS 拉流、cryptography 签名后端跑通真实登录链
❌ 未验证：Windows/Linux 实机（代码层已跨平台，需真机跑一遍）；Linux 的 HLS-only 端口防火墙

## 8. 用户偏好（重要）

- 低延迟直连是核心诉求（当前 2-4ms，勿引入中转）
- 手柄用蓝牙（USB 是 Chromium bug）
- 直播推流 8Mbps 够用（抖音直播伴侣）
- 空闲掉线不管（主机节电设置）
- 喜欢：真实主机图 UI、一键操作、看得懂的统计面板

## 9. 未来路线（用户提过或顺理成章）

- **OpenWrt 部署**：控制面（登录/代理/开关机）→ ipk ~4-5MB，A53 双核可行；直播源需 WebCodecs 端侧编码（浏览器锁 8Mbps 硬编）+ 路由器 ffmpeg 纯 remux（-c copy, 无转码）；PPE 帮不上本机终结流量（已向用户解释）
- **RTMP 直推**：绕开直播伴侣，网页填推流码直接开播（server.py 里已有 ffmpeg 链路，加 -f flv rtmp:// 输出即可）
- **Go 重写 server**：OpenWrt 单二进制 ~8MB 零依赖
- 4K：等微软开放（客户端已验证主机不支持，届时加档位即可）

## 10. 发布流程

```bash
cd <仓库目录>
# 改代码 → 提交
git add -A && git -c user.name=... -c user.email=... commit -m "..."
# 推送 (token 在钥匙串; GitHub 需走代理)
TOKEN=$(security find-generic-password -a xhlbill -s XboxWebRemote-git -w)
git remote set-url origin https://xhlbill:$TOKEN@github.com/xhlbill/XboxWebRemote.git
export https_proxy=http://127.0.0.1:7890
git -c http.proxy=http://127.0.0.1:7890 push -u origin main
git remote set-url origin https://github.com/xhlbill/XboxWebRemote.git
```

⚠️ 历史重写过一次（抹未打码截图），GitHub 对旧 commit 对象的哈希直访可能保留到 GC——
用户已知悉并接受。今后**不要**把敏感内容提交进历史。

## 11. 排障速查（用户常见问题）

| 症状 | 处置 |
|---|---|
| 登录后 5 分钟掉线 | 检查 /api/msal 是否有 token；lpt 是否在 ReadyToConnect 后发送（服务端日志 grep connect）|
| 建会话失败 | 残留会话：DELETE active；或主机被其他设备串流 |
| 手柄无反应 | ① 按 F12 看 gpPill：先点页面再按手柄键；② USB=Chromium bug 用蓝牙；③ 页面失焦输入暂停 |
| 唤醒失败 | 主机节电模式？设置→通用→电源模式=即时启动；或直接 /api/power WakeUp |
| 直播伴侣抓不到声音 | 开直播模式（音频接管到直播窗口/页内视图）后再捕获系统声音 |
| 服务起不来(端口占用) | `pkill -9 -f server.py` + `pkill -f bin/ffmpeg` 后重启 |
