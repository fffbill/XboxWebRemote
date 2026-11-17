// 原生串流会话驱动: 纯 Rust (无浏览器) 连接主机
// 组成: 我们的 XAL 鉴权 (xal.rs) → AuthBundle → nano 引擎 (WebRTC 会话 + 数据通道)
//       → VideoFrameSink (H.264 AU) / AudioFrameSink (Opus) / SessionEventSink (状态)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::engine::auth::{AuthBundle, Platform, StreamingTokenSnapshot, WebTokenSnapshot};
use crate::engine::backend::{AndroidSessionConfig, NanoSessionHandle, SessionEventSink};
use crate::engine::protocol::{default_stream_request, CodecProfile, StreamRequest, VideoProfile};
use crate::engine::session::NanoWebRtcSession;
use crate::engine::stats::StreamStats;
use crate::engine::video_sink::VideoFrameSink;
use crate::engine::audio_sink::AudioFrameSink;
use crate::engine::channels::RumbleData;
use crate::xal;
use crate::{log, App};

struct CountingVideoSink {
    count: AtomicU64,
    bytes: AtomicU64,
}

impl VideoFrameSink for CountingVideoSink {
    fn push_h264_access_unit(&self, data: &[u8], _pts_us: i64, keyframe: bool) {
        let c = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        let b = self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
        if c == 1 || c % 300 == 0 {
            log!(
                "📹 视频帧 #{} ({} 字节, keyframe={}) — 纯 Rust 收到主机画面",
                c,
                data.len(),
                keyframe
            );
        }
        if c % 300 == 0 {
            let secs = c as f64 / 60.0;
            log!("📹 累计: {:.0}秒, {:.1} MB", secs, b as f64 / 1_048_576.0);
        }
    }

    fn drop_pending_output(&self, reason: &str) {
        log!("📹 丢弃待输出帧: {}", reason);
    }
}

struct CountingAudioSink {
    count: AtomicU64,
}

impl AudioFrameSink for CountingAudioSink {
    fn push_opus_frame(&self, data: &[u8], _pts_us: i64, sample_rate: u32, channels: u16) {
        let c = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if c == 1 || c % 500 == 0 {
            log!(
                "🔊 音频帧 #{} ({} 字节, {}Hz {}ch)",
                c,
                data.len(),
                sample_rate,
                channels
            );
        }
    }
}

struct LogEventSink;

impl SessionEventSink for LogEventSink {
    fn update_status(&self, stage: &str, message: &str, terminal: bool) {
        log!("会话[{}] {}{}", stage, message, if terminal { " [终态]" } else { "" });
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

    fn on_rumble(&self, _rumble: crate::engine::channels::RumbleData) {}
}

pub async fn run_connect_test(app: Arc<App>, server_id: &str, duration_secs: u64) -> Result<(), String> {
    // 1. 从我们的 XAL 鉴权层取串流令牌 + lpt
    let (gs_token, host) = xal::get_xhome_token(app.clone(), false).await?;
    let lpt = xal::get_lpt(app.clone()).await?;
    log!("令牌就绪: gs={}字 lpt={}字 host={}", gs_token.len(), lpt.len(), host);

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

    // 2. 串流请求 (1080p60 起步, 主机端协商)
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
        audio_sink: Some(Arc::new(CountingAudioSink {
            count: AtomicU64::new(0),
        })),
        event_sink: Some(Arc::new(LogEventSink)),
    };

    // 3. 启动引擎 (内部: play → provisioned → configuration → offer/answer → ICE → 数据通道)
    log!("启动原生串流会话 (serverId={})...", server_id);
    let mut handle = NanoSessionHandle::start(config).map_err(|e| e.to_string())?;

    // 4. 跑指定时长后停止 (输入帧通道演示: 发送一个全零 gamepad 帧)
    for _ in 0..duration_secs {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let _ = handle.stop();
    log!("连接测试结束");
    Ok(())
}


