// Xbox 串流引擎 — 移植自 Geocld/XStreaming 的 nano-rs (MIT License)
// 纯 Rust WebRTC 串流核心: 信令状态机 / 数据通道 / 输入协议 / 媒体接收

pub mod audio_sink;
pub mod backend;
pub mod auth;
pub mod channels;
pub mod connection;
pub mod error;
pub mod protocol;
pub mod session;
pub mod signaling;
pub mod stats;
pub mod transport;
pub mod video_sink;
pub mod webrtc_backend;

/// 引擎日志宏 (原 logging.rs 由 build.rs 生成, 此处内联等价实现)
#[macro_export]
macro_rules! nano_log {
    ($fmt:literal $(, $arg:expr)*) => {
        println!("[NANO] {}", format!($fmt $(, $arg)*))
    };
}

#[macro_export]
macro_rules! nano_warn {
    ($fmt:literal $(, $arg:expr)*) => {
        println!("[NANO][W] {}", format!($fmt $(, $arg)*))
    };
}

#[macro_export]
macro_rules! nano_error {
    ($fmt:literal $(, $arg:expr)*) => {
        println!("[NANO][E] {}", format!($fmt $(, $arg)*))
    };
}

pub use auth::{AuthBundle, Platform, StreamingTokenSnapshot, WebTokenSnapshot};
pub use channels::{ChannelKind, ChannelSpec};
pub use connection::{NanoConnection, NanoPeerConnection, NanoSignalingClient};
pub use backend::{AndroidSessionConfig, NanoSessionHandle, SessionEventSink};
pub use error::NanoError;
pub use protocol::{
    default_stream_request, CodecProfile, DeviceInfo, HttpMethod, HttpRequestSpec, IceCandidate,
    IceCandidateInput, IceExchangeBody, IceServer, PeerConnectionPlan, SdpExchangeResponse,
    SdpOfferBody, SdpOfferKind, SessionBlueprint, SessionConfiguration, SessionEndpoints,
    SessionPlayBody, SessionPlayResponse, SessionPlaySettings, SessionStateResponse, StreamRequest,
    TransceiverSpec, VideoProfile,
};
pub use session::{MediaTrackKind, NanoWebRtcSession, SessionEvent, SessionSnapshot, SessionState};
pub use stats::StreamStats;
pub use video_sink::VideoFrameSink;
