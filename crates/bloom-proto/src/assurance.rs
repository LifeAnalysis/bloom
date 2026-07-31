use serde::{Deserialize, Serialize};

/// User-verification strength requested by wallet policy.
///
/// This is public policy metadata. Broker and Signer remain responsible for
/// interpreting and enforcing it; Machine may only use it for display and
/// advisory policy comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceLevel {
    #[default]
    Standard,
    Hardened,
}

impl AssuranceLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Hardened => "hardened",
        }
    }
}
