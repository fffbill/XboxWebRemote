// 视频管线: H.264 AU (Annex-B) → AVCC → AVSampleBufferDisplayLayer (VideoToolbox 硬解, 系统渲染)
// 通过原生 objc_msgSend 包装器操作 AVKit/CoreMedia/ObjC (objc2 类型化 msg_send 对未类型化对象限制过多)

use core::ffi::{c_int, c_void, CStr};
use std::sync::{Arc, Mutex};

use objc2::ffi::objc_msgSend;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send};

use crate::engine::video_sink::VideoFrameSink;

// ---------------------------------------------------------------- 类型化 msgSend (ARM64 ABI)

type IdMsg = unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject;
type RectMsg = unsafe extern "C" fn(*mut AnyObject, Sel) -> CGRect;
type VoidIdArgMsg = unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject);
type VoidBoolArgMsg = unsafe extern "C" fn(*mut AnyObject, Sel, bool);
type VoidU64ArgMsg = unsafe extern "C" fn(*mut AnyObject, Sel, u64);
type VoidRectArgMsg = unsafe extern "C" fn(*mut AnyObject, Sel, CGRect);
type IdClassMsg = unsafe extern "C" fn(&AnyClass, Sel) -> *mut AnyObject;

unsafe fn msg_bounds(receiver: *mut AnyObject) -> CGRect {
    let f: RectMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"bounds"))
}

unsafe fn msg_set_frame(receiver: *mut AnyObject, frame: CGRect) {
    let f: VoidRectArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"setFrame:"), frame)
}

unsafe fn msg_set_wants_layer(receiver: *mut AnyObject, v: bool) {
    let f: VoidBoolArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"setWantsLayer:"), v)
}

unsafe fn msg_set_autoresizing(receiver: *mut AnyObject, v: u64) {
    let f: VoidU64ArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"setAutoresizingMask:"), v)
}

unsafe fn msg_add_subview(receiver: *mut AnyObject, sub: *mut AnyObject) {
    let f: VoidIdArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"addSubview:"), sub)
}

unsafe fn msg_set_layer(receiver: *mut AnyObject, layer: *mut AnyObject) {
    let f: VoidIdArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"setLayer:"), layer)
}

unsafe fn msg_set_control_timebase(receiver: *mut AnyObject, tb: *mut c_void) {
    let f: VoidIdArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"setControlTimebase:"), tb as *mut AnyObject)
}

unsafe fn msg_class_alloc(class_ptr: &AnyClass) -> *mut AnyObject {
    let f: IdClassMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(class_ptr, sel(c"alloc"))
}

unsafe fn msg_class_layer(class_ptr: &AnyClass) -> *mut AnyObject {
    let f: IdClassMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(class_ptr, sel(c"layer"))
}

unsafe fn msg_init_with_frame(alloc: *mut AnyObject, frame: CGRect) -> *mut AnyObject {
    let f: unsafe extern "C" fn(*mut AnyObject, Sel, CGRect) -> *mut AnyObject =
        std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(alloc, sel(c"initWithFrame:"), frame)
}

unsafe fn msg_enqueue(receiver: *mut AnyObject, sample: *mut c_void) {
    let f: VoidIdArgMsg = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(receiver, sel(c"enqueueSampleBuffer:"), sample as *mut AnyObject)
}


use crate::{log, App};

// ---------------------------------------------------------------- ObjC 消息发送 (raw)

type VoidIdFn = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
type VoidBoolFn = unsafe extern "C" fn(*mut c_void, Sel, bool);
type VoidU64Fn = unsafe extern "C" fn(*mut c_void, Sel, u64);
type IdFn = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
type RectFn = unsafe extern "C" fn(*mut c_void, Sel) -> CGRect;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn sel(name: &'static CStr) -> Sel {
    Sel::register(name)
}

unsafe fn send_void_id(obj: *mut c_void, s: Sel, arg: *mut c_void) {
    let f: VoidIdFn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, s, arg)
}
unsafe fn send_void_bool(obj: *mut c_void, s: Sel, arg: bool) {
    let f: VoidBoolFn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, s, arg)
}
unsafe fn send_void_u64(obj: *mut c_void, s: Sel, arg: u64) {
    let f: VoidU64Fn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, s, arg)
}
unsafe fn send_id(obj: *mut c_void, s: Sel) -> *mut c_void {
    let f: IdFn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, s)
}
unsafe fn send_rect(obj: *mut c_void, s: Sel) -> CGRect {
    let f: RectFn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, s)
}

unsafe fn send_enqueue(obj: *mut c_void, sample: *mut c_void) {
    let f: VoidIdFn = std::mem::transmute(objc_msgSend as *const c_void as *const ());
    f(obj, sel(c"enqueueSampleBuffer:"), sample)
}

// ---------------------------------------------------------------- CoreMedia FFI

type CFAllocatorRef = *mut c_void;
type CMVideoFormatDescriptionRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CMClockRef = *mut c_void;
type CMTimebaseRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    fn invalid() -> Self {
        CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 }
    }
    fn new(value: i64, timescale: i32) -> Self {
        CMTime { value, timescale, flags: 1, epoch: 0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_timestamp: CMTime,
    decode_timestamp: CMTime,
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> c_int;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: CMBlockBufferRef,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        new_block_buffer_out: *mut CMBlockBufferRef,
    ) -> c_int;
    fn CMBlockBufferReplaceDataBytes(
        source_bytes: *const c_void,
        destination_buffer: CMBlockBufferRef,
        offset_into_destination: usize,
        data_length: usize,
    ) -> c_int;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        format_description: CMVideoFormatDescriptionRef,
        sample_count: i64,
        sample_timing_entry_count: usize,
        sample_timing_array: *const CMSampleTimingInfo,
        sample_size_entry_count: usize,
        sample_size_array: *const usize,
        data_buffer: CMBlockBufferRef,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> c_int;
    fn CMClockGetHostTimeClock() -> CMClockRef;
    fn CMClockGetTime(clock: CMClockRef) -> CMTime;
    fn CMTimebaseCreateWithSourceClock(
        allocator: CFAllocatorRef,
        source_clock: CMClockRef,
        timebase_out: *mut CMTimebaseRef,
    ) -> c_int;
    fn CMTimebaseSetTime(timebase: CMTimebaseRef, time: CMTime);
    fn CMTimebaseSetRate(timebase: CMTimebaseRef, rate: f64);
}

// ---------------------------------------------------------------- 管线

pub struct VideoPipeline {
    display_layer: Retained<AnyObject>, // AVSampleBufferDisplayLayer
    video_view: *mut c_void,            // 承载层
    format: CMVideoFormatDescriptionRef,
    timebase: CMTimebaseRef,
    first_pts_us: Option<i64>,
    last_pts_us: Option<i64>,
    au_count: u64,
}

// 显示层入队线程安全 (Apple 文档); NSView 操作仅限主线程 (create 在主线程调用)
unsafe impl Send for VideoPipeline {}
unsafe impl Sync for VideoPipeline {}

impl VideoPipeline {
    /// 创建视频 NSView + 显示层, 挂到窗口 contentView 上 (必须在主线程调用)
    /// 返回 (管线, 视频视图指针) — 视图指针供窗口切换显示/隐藏
    pub unsafe fn create(container: *mut c_void) -> Result<(Arc<Mutex<Self>>, *mut c_void), String> {
        let cls = objc2::runtime::AnyClass::get(c"NSView").ok_or("NSView 类缺失")?;
        let alloc: *mut AnyObject = msg_class_alloc(cls);
        let bounds: CGRect = msg_bounds(container as *mut AnyObject);
        let video_view: *mut AnyObject = msg_init_with_frame(alloc, bounds);
        if video_view.is_null() {
            return Err("NSView 创建失败".into());
        }
        msg_set_wants_layer(video_view, true);
        msg_add_subview(container as *mut AnyObject, video_view);
        msg_set_autoresizing(video_view, 18); // width|height

        // AVSampleBufferDisplayLayer 挂为 video_view 的层
        let disp_cls = class!(AVSampleBufferDisplayLayer);
        let disp_layer: *mut AnyObject = msg_class_layer(disp_cls);
        msg_set_layer(video_view, disp_layer);
        let b = msg_bounds(video_view);
        msg_set_frame(disp_layer, b);

        // 时基: 主机时钟实时
        let mut timebase: CMTimebaseRef = std::ptr::null_mut();
        let host_clock = CMClockGetHostTimeClock();
        let rc = CMTimebaseCreateWithSourceClock(std::ptr::null_mut(), host_clock, &mut timebase);
        if rc != 0 || timebase.is_null() {
            return Err(format!("CMTimebase 创建失败 ({})", rc));
        }
        let now = CMClockGetTime(host_clock);
        CMTimebaseSetTime(timebase, now);
        CMTimebaseSetRate(timebase, 1.0);
        msg_set_control_timebase(disp_layer as *mut AnyObject, timebase);

        Ok((
            Arc::new(Mutex::new(Self {
                display_layer: unsafe { Retained::from_raw(disp_layer).unwrap() },
                video_view: video_view as *mut c_void,
                format: std::ptr::null_mut(),
                timebase,
                first_pts_us: None,
                last_pts_us: None,
                au_count: 0,
            })),
            video_view as *mut c_void,
        ))
    }

    /// 入队 H.264 AU (Annex-B)
    pub fn push_au(&mut self, au: &[u8], pts_us: i64, _keyframe: bool) {
        let nals = split_annex_b(au);
        if nals.is_empty() {
            return;
        }

        // 提取 SPS(7)/PPS(8) → (重)建格式描述
        let mut sps: Option<&[u8]> = None;
        let mut pps: Option<&[u8]> = None;
        for (data, t) in &nals {
            match t {
                7 => sps = Some(data),
                8 => pps = Some(data),
                _ => {}
            }
        }
        if self.format.is_null() {
            if let (Some(s), Some(p)) = (sps, pps) {
                self.format = create_h264_format(s, p).unwrap_or(std::ptr::null_mut());
            }
        }
        if self.format.is_null() {
            return;
        }

        // Annex-B → AVCC (4 字节长度前缀)
        let mut avcc: Vec<u8> = Vec::with_capacity(au.len());
        for (data, _) in &nals {
            avcc.extend_from_slice(&(data.len() as u32).to_be_bytes());
            avcc.extend_from_slice(data);
        }

        let first = *self.first_pts_us.get_or_insert(pts_us);
        let offset_us = (pts_us - first).max(0);
        let pts_cm = CMTime::new(offset_us, 1_000_000);
        let dur_us = self
            .last_pts_us
            .map(|p| (pts_us - p).max(3333))
            .unwrap_or(33333);
        self.last_pts_us = Some(pts_us);
        let timing = CMSampleTimingInfo {
            duration: CMTime::new(dur_us, 1_000_000),
            presentation_timestamp: pts_cm,
            decode_timestamp: CMTime::invalid(),
        };

        let mut block: CMBlockBufferRef = std::ptr::null_mut();
        let rc1 = unsafe {
            CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                avcc.len(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                avcc.len(),
                0,
                &mut block,
            )
        };
        if rc1 != 0 {
            return;
        }
        unsafe {
            CMBlockBufferReplaceDataBytes(avcc.as_ptr() as *const c_void, block, 0, avcc.len());
        }

        let mut sample: CMSampleBufferRef = std::ptr::null_mut();
        let size = avcc.len();
        let rc2 = unsafe {
            CMSampleBufferCreateReady(
                std::ptr::null_mut(),
                self.format,
                1,
                1,
                &timing,
                1,
                &size,
                block,
                &mut sample,
            )
        };
        if rc2 != 0 {
            return;
        }

        self.au_count += 1;
        if self.au_count == 1 {
            log!("📹 首帧已入队显示层 — VideoToolbox 硬解启动");
        }
        if self.au_count % 600 == 0 {
            log!("📹 已入队 {} 帧", self.au_count);
        }
        unsafe {
            msg_enqueue(retained_as_ptr(&self.display_layer), sample);
        }
    }

    /// 窗口 resize: 层同步视图尺寸
    pub fn resize(&mut self, width: f64, height: f64) {
        let frame = CGRect {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
        };
        unsafe {
            msg_set_frame(Retained::as_ptr(&self.display_layer) as *mut AnyObject, frame);
        }
    }
}

// ---------------------------------------------------------------- Annex-B 解析

/// Annex-B → [(NAL 数据, NAL 类型)]
pub fn split_annex_b(au: &[u8]) -> Vec<(&[u8], u8)> {
    let mut nals = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    for w in 0..starts.len() {
        let s = starts[w];
        let e = if w + 1 < starts.len() {
            let next = starts[w + 1];
            let prefix = if next >= 4 && au[next - 4..next] == [0, 0, 0, 1] { 4 } else { 3 };
            next - prefix
        } else {
            au.len()
        };
        if e > s {
            let t = au[s] & 0x1F;
            nals.push((&au[s..e], t));
        }
    }
    nals
}

fn create_h264_format(sps: &[u8], pps: &[u8]) -> Result<CMVideoFormatDescriptionRef, String> {
    let ptrs = [sps.as_ptr(), pps.as_ptr()];
    let sizes = [sps.len(), pps.len()];
    let mut format: CMVideoFormatDescriptionRef = std::ptr::null_mut();
    let rc = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            std::ptr::null_mut(),
            2,
            ptrs.as_ptr(),
            sizes.as_ptr(),
            4,
            &mut format,
        )
    };
    if rc != 0 || format.is_null() {
        return Err(format!("CMVideoFormatDescription rc={}", rc));
    }
    Ok(format)
}

// ---------------------------------------------------------------- 实现 VideoFrameSink

pub struct NativeVideoSink {
    pipeline: Arc<Mutex<VideoPipeline>>,
}

fn retained_as_ptr(r: &Retained<AnyObject>) -> *mut AnyObject {
    Retained::as_ptr(r) as *mut AnyObject
}

// 显示层入队线程安全 (Apple 文档); NSView 操作仅在创建时主线程
unsafe impl Send for NativeVideoSink {}
unsafe impl Sync for NativeVideoSink {}

impl VideoFrameSink for NativeVideoSink {
    fn push_h264_access_unit(&self, data: &[u8], pts_us: i64, keyframe: bool) {
        let mut p = self.pipeline.lock().unwrap();
        p.push_au(data, pts_us, keyframe);
    }

    fn drop_pending_output(&self, _reason: &str) {}
}
