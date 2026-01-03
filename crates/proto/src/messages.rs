use serde::{Deserialize, Serialize};

/// Messages sent from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Register { token: String, subdomain: String },
    Ping,
    Disconnect,
}

/// Messages sent from server to client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Registered { subdomain: String, url: String },
    Error { code: ErrorCode, message: String },
    Pong,
    Ping,
    CertificateStatus { ready: bool },
    Shutdown { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidToken,
    SubdomainTaken,
    SubdomainInvalid,
    TunnelLimitReached,
    InternalError,
}

impl ClientMessage {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

impl ServerMessage {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_serialization() {
        let msg = ClientMessage::Register {
            token: "tk_abc123".to_string(),
            subdomain: "myapp".to_string(),
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("register"));
        let parsed = ClientMessage::from_json(&json).unwrap();
        match parsed {
            ClientMessage::Register { token, subdomain } => {
                assert_eq!(token, "tk_abc123");
                assert_eq!(subdomain, "myapp");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_serialization() {
        let msg = ServerMessage::Registered {
            subdomain: "myapp".to_string(),
            url: "http://myapp.localhost:8080".to_string(),
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("registered"));

        let err = ServerMessage::error(ErrorCode::InvalidToken, "Bad token");
        let json = err.to_json().unwrap();
        assert!(json.contains("invalid_token"));
    }
}
