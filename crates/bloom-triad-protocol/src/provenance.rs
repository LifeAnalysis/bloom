use serde::{Deserialize, Serialize};

use crate::{Digest32, Token};

/// Trusted runtime subject assertion supplied by authenticated Machine.
/// Broker resolves this subject against its own installer-signed catalog; the
/// Machine never supplies a record or signature for Broker to accept.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProvenanceSubject {
    Petal {
        package_hash: Digest32,
        route: String,
    },
    Cli {
        client_id: Token,
        command_class: Token,
    },
    System {
        component_id: Token,
        operation_class: Token,
    },
}
