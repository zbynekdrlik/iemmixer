//! What a client sends (`type`-tagged): its hello with its role, its
//! requests, and how the engine reads them.

use serde::{Deserialize, Serialize};

use super::cmd::{Cmd, OPS};
use super::engine::{ErrCode, ErrorBody};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Control,
    Observe,
    /// The guard's connection (S6 design note §4): `Shutdown`, `SaveNow`,
    /// `Arm`, the test signals and fault injection (under their launch
    /// flags) and reads; never a mix change. One at a time, beside the
    /// controller: a new supervisor supersedes the old one only.
    Supervisor,
}

// One message at a time, on the control thread only (never the RT thread):
// its size does not matter, and boxing the command would only add an
// allocation per request.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        proto: u16,
        role: Role,
        #[serde(default)]
        client: String,
    },
    Request {
        id: u64,
        /// The sender's session tag, echoed in the resulting `Delta` (echo suppression).
        #[serde(default)]
        origin: Option<u64>,
        cmd: Cmd,
    },
}

/// Longest error text echoed back (a message may quote the offending input).
pub(super) const MAX_MSG: usize = 200;

pub(super) fn short(text: String) -> String {
    if text.len() <= MAX_MSG {
        return text;
    }
    let mut end = MAX_MSG;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn error(code: ErrCode, msg: String) -> ErrorBody {
    ErrorBody {
        code,
        msg: short(msg),
    }
}

#[derive(Deserialize)]
struct Probe {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<u64>,
    cmd: Option<serde_json::Value>,
}

/// Parses one client message. A request the engine cannot read is answered
/// with its id when the id is readable: `Unsupported` for an unknown `op`,
/// `BadRequest` for anything else malformed.
pub fn parse_client(bytes: &[u8]) -> Result<ClientMsg, (Option<u64>, ErrorBody)> {
    let err = match serde_json::from_slice::<ClientMsg>(bytes) {
        Ok(msg) => return Ok(msg),
        Err(e) => e.to_string(),
    };
    let Ok(probe) = serde_json::from_slice::<Probe>(bytes) else {
        return Err((None, error(ErrCode::BadRequest, err)));
    };
    let op = probe
        .cmd
        .as_ref()
        .and_then(|c| c.get("op"))
        .and_then(|op| op.as_str());
    let code = match op {
        Some(op) if probe.kind.as_deref() == Some("request") && !OPS.contains(&op) => {
            ErrCode::Unsupported
        }
        _ => ErrCode::BadRequest,
    };
    Err((probe.id, error(code, err)))
}
