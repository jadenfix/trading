use serde::{Deserialize, Serialize};
use tokio_util::codec::LengthDelimitedCodec;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/openclaw/trading.sock";
pub const MAX_FRAME_LENGTH: usize = 1024 * 1024; // 1 MiB

/// 4-byte Big-Endian length-prefixed JSON codec.
pub type JsonCodec = LengthDelimitedCodec;

pub fn create_codec() -> JsonCodec {
    LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(MAX_FRAME_LENGTH)
        .new_codec()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    pub v: u8,
    pub id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    pub ts_ms: i64,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(kind: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }

    pub fn response_to(req: &Envelope, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: req.id,
            kind: format!("{}.Response", req.kind),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    #[serde(rename = "Control.Start")]
    Start,
    #[serde(rename = "Control.Stop")]
    Stop,
    #[serde(rename = "Control.Status")]
    Status,
    #[serde(rename = "Control.Ping")]
    Ping,
}

impl ControlCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Start => "Control.Start",
            Self::Stop => "Control.Stop",
            Self::Status => "Control.Status",
            Self::Ping => "Control.Ping",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Control.Start" => Some(Self::Start),
            "Control.Stop" => Some(Self::Stop),
            "Control.Status" => Some(Self::Status),
            "Control.Ping" => Some(Self::Ping),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    #[serde(rename = "Event.Alert")]
    Alert { level: String, message: String },
    #[serde(rename = "Event.EngineState")]
    EngineState { running: bool, risk_tripped: bool },
}
