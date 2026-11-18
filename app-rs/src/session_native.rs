// 原生串流会话驱动: 纯 Rust (无浏览器) 连接主机
// 组成: 我们的 XAL 鉴权 (xal.rs) → AuthBundle → nano 引擎 (WebRTC 会话 + 数据通道)
//       → VideoFrameSink / AudioFrameSink / SessionEventSink

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::engine::auth::{AuthBundle, Platform, StreamingTokenSnapshot, WebTokenSnapshot};
use crate::engine::backend::{AndroidSessionConfig, NanoSessionHandle, SessionEventSink};
use crate::engine::channels::{InputFrame, RumbleData};
use crate::engine::protocol::{default_stream_request, CodecProfile, StreamRequest, VideoProfile};
use crate::engine::stats::StreamStats;
use crate::engine::audio_sink::AudioFrameSink;
use crate::engine::video_sink::VideoFrameSink;
use crate::xal;
use crate::{log, App};

pub struct LogEventSink;

impl SessionEventSink for LogEventSink {
    fn update_status(&self, stage: &str, message: &str, terminal: bool) {
        log!(
            "会话[{}] {}{}",
            stage,
            message,
            if terminal { " [终态]" } else { "" }
        );
    }

    fn update_stats(&self, stats: &StreamStats) {
        log!(
            "📊 统计: rtt={:?}ms fps={:?} 码率={:?}bps 丢包={} 帧={}",
            stats.round_trip_time_ms,
            stats.fps,
            stats.bitrate_bps,
            stats.packets_lost,
            stats.frames_received
        );
    }

    fn on_rumble(&self, _rumble: RumbleData) {}
}

/// 构造 AuthBundle (从我们的 XAL 令牌层)
pub async fn build_auth_bundle(app: std::sync::Arc<App>) -> Result<AuthBundle, String> {
    let (gs_token, host) = xal::get_xhome_token(app.to_owned(), false).await?;
    let lpt = xal::get_lpt(app.to_owned()).await?;
    log!(
        "令牌就绪: gs={}字 lpt={}字 host={}",
        gs_token.len(),
        lpt.len(),
        host
    );
    Ok(AuthBundle {
        home: Some(StreamingTokenSnapshot {
            gs_token: gs_token.clone(),
            base_uri: format!("https://{}", host),
            created_at_ms: None,
            expires_at_ms: Some((crate::signing::now() * 1000.0) as u64 + 300_000),
        }),
        cloud: None,
        web: Some(WebTokenSnapshot {
            raw: lpt,
            not_after_ms: None,
        }),
    })
}

/// 构造串流配置 (含渲染/音频/事件 sink; serverId 取第一台主机)
pub async fn build_session_config(
    app: std::sync::Arc<App>,
    video_sink: Option<Arc<dyn VideoFrameSink + Send + Sync>>,
    audio_sink: Option<Arc<dyn AudioFrameSink + Send + Sync>>,
    event_sink: Option<Arc<dyn SessionEventSink + Send + Sync>>,
    height: u32,
) -> Result<AndroidSessionConfig, String> {
    let auth = build_auth_bundle(app.clone()).await?;
    let server_id = first_console_id(app.clone()).await.ok_or("没有已绑定的主机")?;
    let mut request: StreamRequest = default_stream_request(Platform::Home, server_id.clone());
    // base_uri 从 AuthBundle 取 (留空)
    // 分辨率档位: 1080p/1440p 由 osName 协商 (engine 内 os_name_for_resolution)
    request.video = VideoProfile {
        width: if height >= 1440 { 2560 } else { 1920 },
        height: if height >= 1440 { 1440 } else { 1080 },
        fps: 60,
        bitrate_kbps: 20_000,
        codec: CodecProfile::Auto,
        max_operating_rate: 0x7fff,
    };
    Ok(AndroidSessionConfig {
        auth,
        request,
        stereo_audio_enabled: false,
        max_touchpoints: 1,
        coop: false,
        extra_ice_servers: vec![],
        video_sink,
        audio_sink,
        event_sink,
    })
}

async fn first_console_id(app: std::sync::Arc<App>) -> Option<String> {
    let (token, host) = xal::get_xhome_token(app.clone(), false).await.ok()?;
    let url = format!("https://{}/v6/servers/home?mr=50", host);
    let headers = [
        ("Authorization", format!("Bearer {}", token)),
        ("X-Gssv-Client", "XboxComBrowser".into()),
        ("Accept", "application/json".into()),
    ];
    let (status, data) =
        xal::http_json(&app, "GET", &url, &headers, None).await.ok()?;
    if status != 200 {
        return None;
    }
    data.pointer("/results/0/serverId")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// 启动原生会话 (返回句柄供输入注入与停止)
pub async fn start_native_session(
    app: std::sync::Arc<App>,
    video_sink: Option<Arc<dyn VideoFrameSink + Send + Sync>>,
    audio_sink: Option<Arc<dyn AudioFrameSink + Send + Sync>>,
    event_sink: Option<Arc<dyn SessionEventSink + Send + Sync>>,
    height: u32,
) -> Result<NanoSessionHandle, String> {
    let config =
        build_session_config(app, video_sink, audio_sink, event_sink, height).await?;
    crate::engine::backend::NanoSessionHandle::start(config).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 计数 sink (诊断)

pub struct CountingVideoSink {
    pub count: AtomicU64,
    pub bytes: AtomicU64,
}

impl VideoFrameSink for CountingVideoSink {
    fn push_h264_access_unit(&self, data: &[u8], _pts_us: i64, keyframe: bool) {
        let c = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if c == 1 || c % 300 == 0 {
            log!(
                "📹 视频帧 #{} ({} 字节, keyframe={}) — 纯 Rust 收到主机画面",
                c,
                data.len(),
                keyframe
            );
        }
    }

    fn drop_pending_output(&self, _reason: &str) {}
}

pub struct CountingAudioSink;

impl AudioFrameSink for CountingAudioSink {
    fn push_opus_frame(&self, data: &[u8], _pts_us: i64, sample_rate: u32, channels: u16) {
        let _ = (data.len(), sample_rate, channels);
    }
}

/// 无 UI 连接测试 (--connect-test): 计数 sink + 定时停止
pub async fn run_connect_test(
    app: std::sync::Arc<App>,
    server_id: &str,
    duration_secs: u64,
) -> Result<(), String> {
    let (gs_token, host) = xal::get_xhome_token(app.to_owned(), false).await?;
    let lpt = xal::get_lpt(app.to_owned()).await?;
    log!(
        "令牌就绪: gs={}字 lpt={}字 host={}",
        gs_token.len(),
        lpt.len(),
        host
    );

    let auth = AuthBundle {
        home: Some(StreamingTokenSnapshot {
            gs_token: gs_token.clone(),
            base_uri: format!("https://{}", host),
            created_at_ms: None,
            expires_at_ms: Some((crate::signing::now() * 1000.0) as u64 + 300_000),
        }),
        cloud: None,
        web: Some(WebTokenSnapshot {
            raw: lpt.clone(),
            not_after_ms: None,
        }),
    };

    let mut request: StreamRequest = default_stream_request(Platform::Home, server_id);
    request.base_uri = format!("https://{}", host);
    request.video = VideoProfile {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_kbps: 20_000,
        codec: CodecProfile::Auto,
        max_operating_rate: 0x7fff,
    };

    let config = AndroidSessionConfig {
        auth,
        request,
        stereo_audio_enabled: false,
        max_touchpoints: 1,
        coop: false,
        extra_ice_servers: vec![],
        video_sink: Some(Arc::new(CountingVideoSink {
            count: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        })),
        audio_sink: Some(Arc::new(CountingAudioSink)),
        event_sink: Some(Arc::new(LogEventSink)),
    };

    log!("启动原生串流会话 (serverId={})...", server_id);
    let mut handle = crate::engine::backend::NanoSessionHandle::start(config)
        .map_err(|e| e.to_string())?;

    for _ in 0..duration_secs {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let _ = handle.stop();
    log!("连接测试结束");
    Ok(())
}


/// 键盘按键 → 游戏手柄 InputFrame 字段 (与浏览器版 Gamepad keyboard_mapping 一致)
pub fn keyboard_to_frame(key: &str, val: u8) -> InputFrame {
    let mut f = InputFrame::default();
    match key {
        "a" | "Enter" => f.a = val,
        "b" | "Backspace" => f.b = val,
        "x" | "KeyX" => f.x = val,
        "y" | "KeyY" => f.y = val,
        "dpad_up" | "ArrowUp" => f.dpad_up = val,
        "dpad_down" | "ArrowDown" => f.dpad_down = val,
        "dpad_left" | "ArrowLeft" => f.dpad_left = val,
        "dpad_right" | "ArrowRight" => f.dpad_right = val,
        "left_shoulder" | "BracketLeft" => f.left_shoulder = val,
        "right_shoulder" | "BracketRight" => f.right_shoulder = val,
        "left_thumb" | "KeyL" => f.left_thumb = val,
        "right_thumb" | "KeyR" => f.right_thumb = val,
        "menu" | "KeyM" => f.menu = val,
        "nexus" | "KeyN" => f.nexus = val,
        _ => {}
    }
    f
}
