use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{ProtocolError, ProtocolErrorCode};

pub const FRAME_MAX_BYTES: usize = 1024 * 1024;
pub const SINGLE_PAYLOAD_MAX_BYTES: usize = 256 * 1024;
pub const BATCH_CHILD_MAX_BYTES: usize = 64 * 1024;
pub const BATCH_AGGREGATE_MAX_BYTES: usize = 512 * 1024;
pub const BATCH_CHILD_MAX_COUNT: usize = 32;
pub const HPKE_ENVELOPE_MAX_BYTES: usize = 4 * 1024;
pub const JSON_MAX_DEPTH: usize = 32;
pub const JSON_MAX_STRING_BYTES: usize = 256 * 1024;
pub const JSON_MAX_LIST_LENGTH: usize = 256;

/// Encode one canonical JSON value with the four-byte big-endian frame length.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let json = serde_jcs::to_vec(value).map_err(malformed)?;
    if json.len() > FRAME_MAX_BYTES {
        return Err(limit("encoded frame exceeds 1 MiB"));
    }
    let mut frame = Vec::with_capacity(json.len() + 4);
    frame.extend_from_slice(&(json.len() as u32).to_be_bytes());
    frame.extend_from_slice(&json);
    Ok(frame)
}

/// Decode exactly one bounded canonical JSON frame.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, ProtocolError> {
    if frame.len() < 4 {
        return Err(malformed("frame is shorter than its length prefix"));
    }
    let declared = u32::from_be_bytes(frame[..4].try_into().expect("four-byte prefix")) as usize;
    if declared > FRAME_MAX_BYTES {
        return Err(limit("declared frame length exceeds 1 MiB"));
    }
    if frame.len() != declared + 4 {
        return Err(malformed("frame length prefix does not match input"));
    }
    let payload = &frame[4..];
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(malformed)?;
    validate_json_shape(&value, 1)?;
    let canonical = serde_jcs::to_vec(&value).map_err(malformed)?;
    if canonical != payload {
        return Err(malformed("JSON payload is not RFC 8785 canonical"));
    }
    serde_json::from_value(value).map_err(|error| {
        if error.to_string().contains("unknown field") {
            ProtocolError::new(ProtocolErrorCode::UnknownField, error.to_string())
        } else {
            malformed(error)
        }
    })
}

fn validate_json_shape(value: &serde_json::Value, depth: usize) -> Result<(), ProtocolError> {
    if depth > JSON_MAX_DEPTH {
        return Err(limit("JSON nesting depth exceeds 32"));
    }
    match value {
        serde_json::Value::String(value) if value.len() > JSON_MAX_STRING_BYTES => {
            Err(limit("JSON string exceeds 256 KiB"))
        }
        serde_json::Value::Array(values) => {
            if values.len() > JSON_MAX_LIST_LENGTH {
                return Err(limit("JSON list exceeds 256 elements"));
            }
            for value in values {
                validate_json_shape(value, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if key.len() > JSON_MAX_STRING_BYTES {
                    return Err(limit("JSON object key exceeds 256 KiB"));
                }
                validate_json_shape(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A strict unpadded base64url binary value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Base64UrlBytes(String);

impl Base64UrlBytes {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn parse(encoded: impl Into<String>) -> Result<Self, ProtocolError> {
        let encoded = encoded.into();
        if encoded.contains('=') {
            return Err(malformed("base64url values must be unpadded"));
        }
        let decoded = URL_SAFE_NO_PAD.decode(&encoded).map_err(malformed)?;
        if URL_SAFE_NO_PAD.encode(decoded) != encoded {
            return Err(malformed("base64url value is noncanonical"));
        }
        Ok(Self(encoded))
    }

    pub fn decode(&self) -> Vec<u8> {
        URL_SAFE_NO_PAD
            .decode(&self.0)
            .expect("validated base64url")
    }

    pub fn encoded(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Base64UrlBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SigningPayloads {
    Single { payload: Base64UrlBytes },
    Batch { children: Vec<Base64UrlBytes> },
}

impl<'de> Deserialize<'de> for SigningPayloads {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Unchecked {
            Single { payload: Base64UrlBytes },
            Batch { children: Vec<Base64UrlBytes> },
        }

        let payloads = match Unchecked::deserialize(deserializer)? {
            Unchecked::Single { payload } => Self::Single { payload },
            Unchecked::Batch { children } => Self::Batch { children },
        };
        payloads.validate().map_err(serde::de::Error::custom)?;
        Ok(payloads)
    }
}

impl SigningPayloads {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Single { payload } => {
                if payload.decode().len() > SINGLE_PAYLOAD_MAX_BYTES {
                    return Err(limit("single decoded payload exceeds 256 KiB"));
                }
            }
            Self::Batch { children } => {
                if children.is_empty() || children.len() > BATCH_CHILD_MAX_COUNT {
                    return Err(limit("batch must contain 1-32 children"));
                }
                let mut aggregate = 0usize;
                for child in children {
                    let length = child.decode().len();
                    if length > BATCH_CHILD_MAX_BYTES {
                        return Err(limit("decoded batch child exceeds 64 KiB"));
                    }
                    aggregate = aggregate
                        .checked_add(length)
                        .ok_or_else(|| limit("decoded batch aggregate overflow"))?;
                }
                if aggregate > BATCH_AGGREGATE_MAX_BYTES {
                    return Err(limit("decoded batch aggregate exceeds 512 KiB"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HpkeEnvelope {
    pub kem_output: Base64UrlBytes,
    pub ciphertext: Base64UrlBytes,
}

impl<'de> Deserialize<'de> for HpkeEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Unchecked {
            kem_output: Base64UrlBytes,
            ciphertext: Base64UrlBytes,
        }

        let unchecked = Unchecked::deserialize(deserializer)?;
        let envelope = Self {
            kem_output: unchecked.kem_output,
            ciphertext: unchecked.ciphertext,
        };
        envelope.validate().map_err(serde::de::Error::custom)?;
        Ok(envelope)
    }
}

impl HpkeEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let total = self
            .kem_output
            .decode()
            .len()
            .checked_add(self.ciphertext.decode().len())
            .ok_or_else(|| limit("HPKE envelope length overflow"))?;
        if total > HPKE_ENVELOPE_MAX_BYTES {
            return Err(limit("decoded HPKE envelope exceeds 4 KiB"));
        }
        Ok(())
    }
}

fn malformed(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::MalformedFrame, error.to_string())
}

fn limit(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::LimitExceededFrame, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Example {
        a: String,
        z: u8,
    }

    #[test]
    fn canonical_frame_round_trips_and_noncanonical_input_fails() {
        let value = Example {
            a: "ok".into(),
            z: 7,
        };
        let frame = encode_frame(&value).unwrap();
        assert_eq!(decode_frame::<Example>(&frame).unwrap(), value);

        let json = br#"{"z":7,"a":"ok"}"#;
        let mut noncanonical = (json.len() as u32).to_be_bytes().to_vec();
        noncanonical.extend_from_slice(json);
        assert_eq!(
            decode_frame::<Example>(&noncanonical).unwrap_err().code,
            ProtocolErrorCode::MalformedFrame
        );
    }

    #[test]
    fn each_payload_bound_is_independent() {
        let single = SigningPayloads::Single {
            payload: Base64UrlBytes::from_bytes(&vec![0; SINGLE_PAYLOAD_MAX_BYTES + 1]),
        };
        assert!(single.validate().is_err());

        let child = SigningPayloads::Batch {
            children: vec![Base64UrlBytes::from_bytes(&vec![
                0;
                BATCH_CHILD_MAX_BYTES + 1
            ])],
        };
        assert!(child.validate().is_err());

        let aggregate = SigningPayloads::Batch {
            children: vec![
                Base64UrlBytes::from_bytes(&vec![0; BATCH_CHILD_MAX_BYTES]);
                BATCH_CHILD_MAX_COUNT
            ],
        };
        let error = aggregate.validate().unwrap_err();
        assert!(error.message.contains("aggregate"));

        let count = SigningPayloads::Batch {
            children: vec![Base64UrlBytes::from_bytes(&[]); BATCH_CHILD_MAX_COUNT + 1],
        };
        assert!(count.validate().unwrap_err().message.contains("1-32"));
    }
}
