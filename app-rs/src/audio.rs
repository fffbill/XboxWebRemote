// 音频管线: libopus 解码 (vendored 静态编译) → cpal 播放
// nano-rs AudioFrameSink 交付原始 Opus 帧 (48kHz 立体声)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::engine::audio_sink::AudioFrameSink;
use crate::{log, App};

// ---------------------------------------------------------------- libopus FFI

unsafe extern "C" {
    fn opus_decoder_create(Fs: i32, channels: i32, error: *mut i32) -> *mut core::ffi::c_void;
    fn opus_decode(
        dec: *mut core::ffi::c_void,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
    fn opus_decoder_destroy(dec: *mut core::ffi::c_void);
}

// ---------------------------------------------------------------- 环形缓冲

const RING_CAP: usize = 48_000 * 2; // 500ms 立体声 f32

struct Ring {
    buf: Vec<f32>,
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl Ring {
    fn new() -> Self {
        Ring {
            buf: vec![0.0; RING_CAP],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, data: &[f32]) -> usize {
        let mut pushed = 0usize;
        for &v in data {
            if self.count == RING_CAP {
                break; // 满: 丢新数据 (播放端消费更慢时)
            }
            self.buf[self.write_pos] = v;
            self.write_pos = (self.write_pos + 1) % RING_CAP;
            self.count += 1;
            pushed += 1;
        }
        pushed
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let mut n = 0usize;
        for o in out.iter_mut() {
            if self.count == 0 {
                *o = 0.0; // 欠载: 静音
                n += 1;
                continue;
            }
            *o = self.buf[self.read_pos];
            self.read_pos = (self.read_pos + 1) % RING_CAP;
            self.count -= 1;
            n += 1;
        }
        n
    }

    fn buffered(&self) -> usize {
        self.count
    }
}

// ---------------------------------------------------------------- 管线

pub struct AudioPipeline {
    decoder: *mut core::ffi::c_void,
    pcm_buf: Vec<i16>,
    ring: Arc<Mutex<Ring>>,
    stream: Option<cpal::Stream>,
    last_underrun_log: std::sync::Mutex<f64>,
}

impl AudioPipeline {
    pub fn new() -> Result<Self, String> {
        // 1. opus 解码器 (48kHz 立体声)
        let mut err: i32 = 0;
        let decoder = unsafe { opus_decoder_create(48000, 2, &mut err) };
        if err != 0 || decoder.is_null() {
            return Err(format!("opus 解码器创建失败 (err={})", err));
        }

        // 2. cpal 输出流
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "无默认音频输出设备".to_string())?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("获取输出配置失败: {}", e))?;
        let sample_rate: u32 = supported.sample_rate().into();
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        log!(
            "音频输出: {}Hz {}ch {:?}",
            sample_rate,
            channels,
            sample_format
        );

        let ring = Arc::new(Mutex::new(Ring::new()));
        let ring_cb = ring.clone();
        let config: cpal::StreamConfig = supported.into();

        let err_fn = |err| log!("cpal 输出流错误: {}", err);
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config.clone(),
                move |out: &mut [f32], _| {
                    let mut r = ring_cb.lock().unwrap();
                    r.read(out);
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                config.clone(),
                move |out: &mut [i16], _| {
                    let mut r = ring_cb.lock().unwrap();
                    for o in out.iter_mut() {
                        let mut s = [0f32; 1];
                        r.read(&mut s);
                        *o = (s[0].clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(format!("不支持的采样格式: {:?}", other));
            }
        }
        .map_err(|e| format!("输出流创建失败: {}", e))?;
        stream.play().map_err(|e| format!("输出流启动失败: {}", e))?;

        Ok(Self {
            decoder,
            pcm_buf: vec![0i16; 5760 * channels.max(1)],
            ring,
            stream: Some(stream),
            last_underrun_log: std::sync::Mutex::new(0.0),
        })
    }

    /// 解码一帧 opus 并推入播放缓冲
    pub fn push_opus_frame(&mut self, data: &[u8]) {
        let channels = self.pcm_buf.len() / 5760.max(1).max(1) / 1; // 占位
        let _ = channels;
        let samples_per_ch = unsafe {
            opus_decode(
                self.decoder,
                if data.is_empty() { std::ptr::null() } else { data.as_ptr() },
                data.len() as i32,
                self.pcm_buf.as_mut_ptr(),
                5760, // 最大单帧采样数 (每通道)
                0,
            )
        };
        if samples_per_ch <= 0 {
            return;
        }
        // i16 → f32 交错
        let total = samples_per_ch as usize * self.channels();
        let mut f32_buf = Vec::with_capacity(total);
        for i in 0..total {
            f32_buf.push(self.pcm_buf[i] as f32 / 32768.0);
        }
        // 抖动控制: 缓冲过量 (超过 300ms) 时丢弃整帧
        let over = {
            let r = self.ring.lock().unwrap();
            r.buffered() > 48_000 * 2 * 3 / 10 // 300ms 立体声
        };
        if over {
            return;
        }
        let mut r = self.ring.lock().unwrap();
        r.push(&f32_buf);
    }

    fn channels(&self) -> usize {
        2
    }
}

unsafe impl Send for AudioPipeline {}
unsafe impl Sync for AudioPipeline {}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stream = None; // 先停流
        unsafe { opus_decoder_destroy(self.decoder) };
    }
}

// ---------------------------------------------------------------- 实现 AudioFrameSink

/// 适配 nano 引擎的 AudioFrameSink: 解码并播放 (内部可变性)
pub struct NativeAudioSink {
    pipeline: Mutex<AudioPipeline>,
    count: AtomicU64,
}

impl NativeAudioSink {
    pub fn new() -> Result<Arc<Self>, String> {
        let pipeline = AudioPipeline::new()?;
        Ok(Arc::new(Self {
            pipeline: Mutex::new(pipeline),
            count: AtomicU64::new(0),
        }))
    }
}

impl AudioFrameSink for NativeAudioSink {
    fn push_opus_frame(&self, data: &[u8], _pts_us: i64, sample_rate: u32, channels: u16) {
        let _ = (sample_rate, channels); // 引擎固定 48k/2ch, 解码器按此创建
        let mut p = self.pipeline.lock().unwrap();
        p.push_opus_frame(data);
        let c = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if c == 1 || c % 1000 == 0 {
            log!("🔊 音频已解码播放: #{} 帧", c);
        }
    }
}

// ---------------------------------------------------------------- 挂接

pub fn attach_to_app(app: &App) -> Result<Arc<NativeAudioSink>, String> {
    NativeAudioSink::new()
}
