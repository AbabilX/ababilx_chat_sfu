//! A small WebRTC SFU for group audio and screen sharing.
//!
//! The forwarding path never inspects an RTP payload. Everything the SFU needs
//! to route — audio level, keyframe requests, congestion feedback — travels in
//! RTP headers and RTCP, so an application can encrypt payloads end-to-end
//! without the server needing to know or care.
pub mod auth;
pub mod config;
pub mod proto;
pub mod report;
pub mod sfu;
pub mod signal;
