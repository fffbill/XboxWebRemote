use bytes::Bytes;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;

use crate::{log, App};

pub const TMP_DIR_NAME: &str = "xwr-hls";

#[derive(Default)]
pub struct LiveMeta {
    pub child: Option<tokio::process::Child>,
    pub sid: Option<String>,
    pub head: Vec<u8>, // 接入缓冲: FLV头+配置tag+最近关键帧
    pub in_bytes: u64,
}

pub struct Live {
    // 元数据锁: 短临界区 (无 await 跨越), reader 高频更新安全
    pub meta: std::sync::Mutex<LiveMeta>,
    // stdin 写锁: 独立 (写入可长时间阻塞, 绝不与元数据锁互等, 否则死锁)
    pub stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    pub tx: std::sync::Mutex<Option<broadcast::Sender<Vec<u8>>>>,
    pub encoder: std::sync::Mutex<Option<(String, Vec<String>)>>,
}

impl Live {
    pub fn new() -> Self {
        Live {
            meta: std::sync::Mutex::new(LiveMeta::default()),
            stdin: tokio::sync::Mutex::new(None),
            tx: std::sync::Mutex::new(None),
            encoder: std::sync::Mutex::new(None),
        }
    }

    fn is_running(child: &mut Option<tokio::process::Child>) -> bool {
        child
            .as_mut()
            .map(|c| c.try_wait().map(|s| s.is_none()).unwrap_or(false))
            .unwrap_or(false)
    }
}

pub fn hls_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(TMP_DIR_NAME)
}

pub fn ffmpeg_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("xwr-ffmpeg.log")
}

pub fn lan_ip() -> String {
    use std::net::UdpSocket;
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".into()
}

fn find_ffmpeg() -> String {
    let names: &[&str] = if cfg!(windows) {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg"]
    };
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf()); // Contents/MacOS 或 target/debug
            dirs.push(d.parent().unwrap_or(d).join("Resources")); // .app Resources
        }
    }
    dirs.push(crate::base_dir()); // cwd (桌面文件夹运行)
    for d in dirs {
        for n in names {
            let cand = d.join("bin").join(n);
            if cand.is_file() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    "ffmpeg".into()
}

fn select_encoder(ff: &str, cache: &std::sync::Mutex<Option<(String, Vec<String>)>>) -> (String, Vec<String>) {
    let mut c = cache.lock().unwrap();
    if let Some(hit) = &*c {
        return hit.clone();
    }
    let mut name = "libx264".to_string();
    let mut args = vec![
        "-c:v".into(), "libx264".into(),
        "-preset".into(), "veryfast".into(),
        "-tune".into(), "zerolatency".into(),
        "-b:v".into(), "8M".into(),
        "-maxrate".into(), "8.5M".into(),
        "-bufsize".into(), "16M".into(),
    ];
    if let Ok(o) = std::process::Command::new(ff).arg("-hide_banner").arg("-encoders").output() {
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        let candidates: [(&str, Vec<&str>); 4] = [
            ("h264_videotoolbox", vec!["-c:v", "h264_videotoolbox", "-allow_sw", "1", "-realtime", "1", "-b:v", "8M", "-maxrate", "8.5M", "-bufsize", "16M"]),
            ("h264_nvenc", vec!["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-b:v", "8M", "-maxrate", "8.5M", "-bufsize", "16M"]),
            ("h264_qsv", vec!["-c:v", "h264_qsv", "-b:v", "8M", "-maxrate", "8.5M", "-bufsize", "16M"]),
            ("h264_amf", vec!["-c:v", "h264_amf", "-usage", "lowlatency", "-b:v", "8M", "-maxrate", "8.5M", "-bufsize", "16M"]),
        ];
        for (cand, cargs) in candidates {
            if text.contains(cand) {
                name = cand.to_string();
                args = cargs.iter().map(|s| s.to_string()).collect();
                break;
            }
        }
    }
    log!("直播源 H.264 编码器: {}", name);
    *c = Some((name.clone(), args.clone()));
    (name, args)
}

pub async fn start(app: std::sync::Arc<App>) -> serde_json::Value {
    let ff = find_ffmpeg();
    let (already, sid_old) = {
        let mut meta = app.live.meta.lock().unwrap();
        let already = Live::is_running(&mut meta.child);
        let sid = meta.sid.clone().unwrap_or_default();
        // 清理已退出的旧进程记录
        if let Some(c) = meta.child.as_mut() {
            if let Ok(Some(_)) = c.try_wait() {
                meta.child = None;
            }
        }
        (already, sid)
    };
    if already {
        return json!({"ok": true, "already": true, "sid": sid_old});
    }

    let hls_dir = hls_dir();
    let _ = std::fs::create_dir_all(&hls_dir);
    let hls_path = hls_dir.join("live.m3u8");
    let (_enc_name, enc_args) = select_encoder(&ff, &app.live.encoder);
    // 注意: ffmpeg 输出选项只作用于紧随其后的输出, 两路输出必须各自声明编码
    let mut cmd = tokio::process::Command::new(&ff);
    cmd.args([
        "-hide_banner", "-loglevel", "info",
        "-fflags", "+genpts", "-f", "matroska", "-i", "pipe:0",
        "-map", "0:v:0", "-map", "0:a:0",
    ]);
    cmd.args(&enc_args);
    cmd.args([
        "-c:a", "aac", "-b:a", "128k",
        "-f", "flv", "pipe:1",
        "-map", "0:v:0", "-map", "0:a:0",
        "-c:v", "copy", "-c:a", "aac", "-b:a", "128k",
        "-f", "hls", "-hls_time", "2", "-hls_list_size", "8",
        "-hls_flags", "delete_segments+independent_segments",
        "-hls_segment_filename",
    ]);
    cmd.arg(hls_dir.join("seg_%04d.ts"));
    cmd.arg(&hls_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::from(
        std::fs::File::create(ffmpeg_log_path()).unwrap_or_else(|_| {
            std::fs::File::create(std::path::Path::new("/dev/null")).unwrap()
        }),
    ));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": format!("启动 ffmpeg 失败: {}", e)}),
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    *app.live.stdin.lock().await = stdin;
    let sid = uuid::Uuid::new_v4().to_string();
    {
        let mut meta = app.live.meta.lock().unwrap();
        meta.child = Some(child);
        meta.sid = Some(sid.clone());
        meta.head = Vec::new();
        meta.in_bytes = 0;
    }
    let (tx, _) = broadcast::channel::<Vec<u8>>(2000);
    *app.live.tx.lock().unwrap() = Some(tx.clone());
    if let Some(mut out) = stdout {
        let app2 = app.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            reader_loop(&app2, &tx2, &mut out).await;
            // ffmpeg 自然结束: 清运行标记但保留 head 缓冲, 晚到的拉流端仍可消费最后一段
            // (显式 stop 才清空 head)
            let mut meta = app2.live.meta.lock().unwrap();
            if meta.child.is_some() {
                meta.child = None;
                meta.sid = None;
            }
        });
    }
    log!("直播源已启动 (HTTP-FLV: /live.flv)");
    json!({"ok": true, "sid": sid})
}

/// FLV tag 级解析 (Python _live_reader 移植):
/// 接入缓冲 = FLV头+配置tag(AVC/AAC seq header)+最近关键帧起 tags, 拼接处修正 prev-size
async fn reader_loop(
    app: &App,
    tx: &broadcast::Sender<Vec<u8>>,
    stdout: &mut (impl tokio::io::AsyncRead + Unpin),
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut kf: Vec<u8> = Vec::new();
    let mut config: Vec<u8> = Vec::new();
    let mut config_last_total: usize = 0;
    let mut header_seen = false;
    let mut chunk = vec![0u8; 16384];
    loop {
        let n = match tokio::io::AsyncReadExt::read(stdout, &mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut pos = 0usize;
        if !header_seen {
            // FLV 头 (9B) + PreviousTagSize0 (4B, 值为 0) 必须一起进配置段,
            // 否则第一个 tag 的 prev-size 校验会失败
            if buf.len() < 13 {
                continue;
            }
            config = buf[..13].to_vec();
            header_seen = true;
            pos = 13;
        }
        while buf.len() - pos >= 15 {
            let tagtype = buf[pos + 4];
            let datasize = ((buf[pos + 5] as usize) << 16)
                | ((buf[pos + 6] as usize) << 8)
                | buf[pos + 7] as usize;
            let total = 4 + 11 + datasize;
            if buf.len() - pos < total {
                break;
            }
            let mut tag = buf[pos..pos + total].to_vec();
            if tagtype == 18 {
                // onMetaData 等脚本 tag: 归入配置段
                config.extend_from_slice(&tag);
                config_last_total = total;
            } else if tagtype == 9 {
                let b0 = buf[pos + 15];
                let b1 = if total > 16 { buf[pos + 16] } else { 0 };
                if (b0 & 0xF0) == 0x10 && b1 == 0x00 {
                    config.extend_from_slice(&tag); // AVC sequence header
                    config_last_total = total;
                } else if (b0 & 0xF0) == 0x10 && b1 == 0x01 {
                    // 视频关键帧 NAL: 重置接入缓冲, 修正拼接处 prev-size
                    kf = config.clone();
                    tag[0..4].copy_from_slice(&(config_last_total as u32).to_be_bytes());
                    kf.extend_from_slice(&tag);
                } else {
                    kf.extend_from_slice(&tag);
                    if kf.len() > 12 * 1024 * 1024 {
                        kf = config.clone(); // 关键帧间隔异常, 等下一个
                    }
                }
            } else if tagtype == 8 {
                if buf[pos + 15] == 0x00 {
                    config.extend_from_slice(&tag); // AAC sequence header
                    config_last_total = total;
                } else {
                    kf.extend_from_slice(&tag);
                }
            }
            pos += total;
        }
        buf.drain(..pos);
        {
            let mut meta = app.live.meta.lock().unwrap();
            meta.head = kf.clone();
            meta.in_bytes += n as u64;
        }
        let _ = tx.send(chunk[..n].to_vec());
    }
}

pub async fn stop(app: std::sync::Arc<App>) -> serde_json::Value {
    // 关闭 stdin 让 ffmpeg 自然收尾
    {
        let mut guard = app.live.stdin.lock().await;
        if let Some(sin) = guard.as_mut() {
            let _ = sin.shutdown().await;
        }
        *guard = None;
    }
    // 等待退出, 超时强杀
    let mut child = {
        let mut meta = app.live.meta.lock().unwrap();
        meta.child.take()
    };
    if let Some(c) = &mut child {
        match tokio::time::timeout(std::time::Duration::from_secs(3), c.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = c.kill().await;
            }
        }
    }
    let meta_guard = app.live.meta.lock().unwrap();
    drop(meta_guard);
    let dir = hls_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    *app.live.tx.lock().unwrap() = None;
    log!("直播源已停止");
    json!({"ok": true})
}

pub async fn status(app: std::sync::Arc<App>) -> serde_json::Value {
    let (running, in_bytes, clients) = {
        let mut meta = app.live.meta.lock().unwrap();
        let running = Live::is_running(&mut meta.child);
        let clients = app
            .live
            .tx
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.receiver_count())
            .unwrap_or(0);
        (running, meta.in_bytes, clients)
    };
    json!({
        "running": running,
        "clients": clients,
        "in_bytes": in_bytes,
        "lan_ip": lan_ip(),
    })
}

pub async fn feed_chunk(
    app: std::sync::Arc<App>,
    sid_header: Option<String>,
    body: bytes::Bytes,
) -> Result<(), (axum::http::StatusCode, serde_json::Value)> {
    // 会话/存活校验 (短临界区)
    {
        let mut meta = app.live.meta.lock().unwrap();
        let running = Live::is_running(&mut meta.child);
        if !running {
            return Err((
                axum::http::StatusCode::CONFLICT,
                json!({"error": "直播源未启动"}),
            ));
        }
        if let Some(expect) = &meta.sid {
            match &sid_header {
                Some(h) if h != expect => {
                    return Err((
                        axum::http::StatusCode::CONFLICT,
                        json!({"error": "推流会话已更替"}),
                    ));
                }
                _ => {}
            }
        }
    }
    let mut guard = app.live.stdin.lock().await;
    let stdin = guard
        .as_mut()
        .ok_or((
            axum::http::StatusCode::CONFLICT,
            json!({"error": "直播源未启动"}),
        ))?;
    if stdin.write_all(&body).await.is_err() {
        return Err((
            axum::http::StatusCode::BAD_GATEWAY,
            json!({"error": "推流管道已断"}),
        ));
    }
    if stdin.flush().await.is_err() {
        return Err((
            axum::http::StatusCode::BAD_GATEWAY,
            json!({"error": "推流管道已断"}),
        ));
    }
    Ok(())
}

/// /live.flv: 先等关键帧接入点, 再订阅广播收实时数据 (顺序不能反)
pub async fn flv_stream(
    app: std::sync::Arc<App>,
) -> Result<axum::response::Response, (axum::http::StatusCode, serde_json::Value)> {
    {
        let mut meta = app.live.meta.lock().unwrap();
        if !Live::is_running(&mut meta.child) {
            return Err((
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "直播源未启动, 请先在页面开启推流"}),
            ));
        }
    }
    let mut head: Vec<u8> = Vec::new();
    let mut waited = 0.0f64;
    while waited < 20.0 {
        {
            let mut meta = app.live.meta.lock().unwrap();
            if !Live::is_running(&mut meta.child) {
                return Err((
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": "直播源已停止"}),
                ));
            }
            head = meta.head.clone();
        }
        if !head.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        waited += 0.5;
    }
    if head.is_empty() {
        return Err((
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            json!({"error": "等待关键帧超时, 请重新开始推流"}),
        ));
    }
    let tx = app.live.tx.lock().unwrap().clone();
    let tx = tx.ok_or((
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": "直播源未启动"}),
    ))?;
    let mut rx = tx.subscribe();

    let app2 = app.clone();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, std::io::Error>(Bytes::from(head));
        loop {
            match rx.recv().await {
                Ok(data) => {
                    if data.is_empty() { break; }
                    if data == b"RESYNC" {
                        // 拉流端堆积后重同步: 从最近关键帧重新接入
                        let h = { app2.live.meta.lock().unwrap().head.clone() };
                        yield Ok(Bytes::from(h));
                        continue;
                    }
                    yield Ok(Bytes::from(data));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let h = { app2.live.meta.lock().unwrap().head.clone() };
                    yield Ok(Bytes::from(h));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "video/x-flv")
        .header("Cache-Control", "no-store")
        .header("Connection", "close")
        .body(axum::body::Body::from_stream(stream))
        .unwrap())
}
