// 纯原生 Rust 串流 App — 零浏览器依赖
// 窗口: tao (无 WebView)
// 视频: nano-rs 引擎 → VideoFrameSink(H.264 AU) → AVSampleBufferDisplayLayer 硬解渲染
// 音频: nano-rs 引擎 → AudioFrameSink(Opus) → libopus 解码 → cpal 播放
// 手柄: GameController 框架 → InputFrame → 引擎数据通道

use std::sync::{Arc, Mutex};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;

use crate::engine::backend::{AndroidSessionConfig, NanoSessionHandle, SessionEventSink};
use crate::engine::channels::InputFrame;
use crate::engine::protocol::{default_stream_request, CodecProfile, StreamRequest, VideoProfile};
use crate::engine::video_sink::VideoFrameSink;
use crate::engine::audio_sink::AudioFrameSink;
use crate::engine::stats::StreamStats;
use crate::engine::channels::RumbleData;
use crate::xal;
use crate::{log, App};

// ================================================================ 事件

#[derive(Debug, Clone)]
pub enum NativeEvent {
    VideoFrame(u64),           // 视频帧计数
    SessionStatus(String, bool), // 状态消息, 是否终态
}

// ================================================================ 视频渲染

pub struct VideoRenderer {
    display_layer: *mut core::ffi::c_void, // AVSampleBufferDisplayLayer
}

unsafe impl Send for VideoRenderer {}
unsafe impl Sync for VideoRenderer {}

impl VideoRenderer {
    /// 创建显示层并挂到 NSView (ns_view 来自 tao 窗口的 contentView)
    pub unsafe fn new(ns_view: *mut core::ffi::c_void) -> Result<Self, String> {
        use objc2::runtime::AnyClass;
        use objc2::{class, msg_send};

        let disp_cls = class!(AVSampleBufferDisplayLayer);
        let layer: *mut AnyObject = msg_send![disp_cls, layer];

        // 挂到 NSView
        let _: () = msg_send![ns_view, setWantsLayer: true];
        let _: () = msg_send![ns_view, setLayer: layer];

        // 填满视图
        let bounds: objc2::runtime::AnyObject = msg_send![ns_view, bounds];
        let _: () = msg_send![layer, setFrame: &*bounds];

        // 背景黑色
        let _: () = msg_send![layer, setBackgroundColor: objc2::msg_send![class!(CALayer), colorWithCGColor]];

        Ok(Self { display_layer: layer })
    }

    pub fn enqueue_sample(&mut self, sample: *mut core::ffi::c_void) {
        unsafe {
            let _: () = objc2::msg_send![self.display_layer, enqueueSampleBuffer: sample];
        }
    }
}

// ================================================================ 音频输出

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    decoder: *mut core::ffi::c_void, // libopus decoder
    pcm_buf: Vec<i16>,
}

unsafe impl Send for AudioOutput {}

impl AudioOutput {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device()
            .ok_or("无默认音频输出设备")?;
        let config: cpal::StreamConfig = device.default_output_config()
            .map_err(|e| format!("音频配置: {}", e))?
            .into();
        log!("音频输出: {}Hz {}ch", config.sample_rate.0, config.channels);

        let ring = Arc::new(Mutex::new(AudioRing::new()));
        let ring_cb = ring.clone();

        let stream = device.build_output_stream(
            &config,
            move |out: &mut [f32], _| {
                let mut r = ring_cb.lock().unwrap();
                for o in out.iter_mut() {
                    *o = r.pop().unwrap_or(0.0);
                }
            },
            |err| log!("cpal err: {}", err),
            None,
        ).map_err(|e| format!("音频流: {}", e))?;
        stream.play().map_err(|e| format!("音频启动: {}", e))?;

        Ok(Self {
            _stream: Some(stream),
            decoder: std::ptr::null_mut(),
            pcm_buf: Vec::new(),
        })
    }
}

pub struct AudioRing {
    buf: Vec<f32>,
    read: usize,
    write: usize,
    count: usize,
    cap: usize,
}

impl AudioRing {
    pub fn new() -> Self {
        Self { buf: vec![0.0; 96000], read: 0, write: 0, count: 0, cap: 96000 }
    }
    pub fn push(&mut self, data: &[f32]) {
        for &v in data {
            if self.count >= self.cap { return; }
            self.buf[self.write] = v;
            self.write = (self.write + 1) % self.cap;
            self.count += 1;
        }
    }
    pub fn pop(&mut self) -> Option<f32> {
        if self.count == 0 { return None; }
        let v = self.buf[self.read];
        self.read = (self.read + 1) % self.cap;
        self.count -= 1;
        Some(v)
    }
}
