use bloom_triad_protocol::*;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Body {
    value: String,
}

fn signed() -> (SignedEnvelope<Body>, AuthenticatedPeer) {
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let body = Body {
        value: "bound".into(),
    };
    let request_digest =
        Digest32::from_bytes(Sha256::digest(serde_jcs::to_vec(&body).unwrap()).into());
    let unsigned = UnsignedEnvelope {
        protocol: ProtocolVersion::CURRENT,
        schema: Token::new(RPC_ENVELOPE_SCHEMA_V1).unwrap(),
        kind: EnvelopeKind::Request,
        method: Token::new("signer.sign").unwrap(),
        operation_id: OperationId::new("11".repeat(32)).unwrap(),
        request_digest,
        caller_service_id: Token::new("bloom-broker").unwrap(),
        caller_boot_epoch: BootEpoch::new("22".repeat(16)).unwrap(),
        audience: Token::new("bloom-signer").unwrap(),
        sent_at_ms: DecimalU64::new(10),
        deadline_ms: DecimalU64::new(20),
        body,
        application_key_id: Token::new("broker-app-1").unwrap(),
    };
    let signature = signing_key.sign(&unsigned.canonical_bytes().unwrap());
    (
        SignedEnvelope {
            unsigned,
            signature: Base64UrlBytes::from_bytes(&signature.to_bytes()),
        },
        AuthenticatedPeer {
            effective_uid: 501,
            service_id: Token::new("bloom-broker").unwrap(),
            boot_epoch: BootEpoch::new("22".repeat(16)).unwrap(),
            audience: Token::new("bloom-signer").unwrap(),
            application_key_id: Token::new("broker-app-1").unwrap(),
            application_public_key: signing_key.verifying_key().to_bytes(),
        },
    )
}

#[test]
fn ac05_every_peer_and_envelope_binding_fails_closed() {
    let (envelope, peer) = signed();
    envelope.verify(501, &peer).unwrap();

    let mut changed_peer = peer.clone();
    changed_peer.application_key_id = Token::new("other-key").unwrap();
    assert_eq!(
        envelope.verify(501, &changed_peer).unwrap_err().code,
        ProtocolErrorCode::UnauthenticatedPeer
    );
    let mut changed_peer = peer.clone();
    changed_peer.boot_epoch = BootEpoch::new("33".repeat(16)).unwrap();
    assert_eq!(
        envelope.verify(501, &changed_peer).unwrap_err().code,
        ProtocolErrorCode::UnauthenticatedPeer
    );
    let mut changed_peer = peer.clone();
    changed_peer.audience = Token::new("bloom-machine").unwrap();
    assert_eq!(
        envelope.verify(501, &changed_peer).unwrap_err().code,
        ProtocolErrorCode::UnauthenticatedPeer
    );

    let mut changed = envelope.clone();
    changed.unsigned.schema = Token::new("bloom.rpc-envelope.2").unwrap();
    assert_eq!(
        changed.verify(501, &peer).unwrap_err().code,
        ProtocolErrorCode::UnsupportedVersion
    );
    let mut changed = envelope.clone();
    changed.signature = Base64UrlBytes::from_bytes(&[0; 64]);
    assert_eq!(
        changed.verify(501, &peer).unwrap_err().code,
        ProtocolErrorCode::UnauthenticatedPeer
    );
    assert_eq!(
        envelope.verify(502, &peer).unwrap_err().code,
        ProtocolErrorCode::UnauthenticatedPeer
    );
}

fn enrolled() -> EnrolledKeyBinding {
    EnrolledKeyBinding {
        key_ref: KeyRef {
            backend: Token::new("local").unwrap(),
            backend_instance: Token::new("local-default").unwrap(),
            locator: "key-1".into(),
            key_spec: KeySpec::Secp256k1,
            public_key_fingerprint: Digest32::new("44".repeat(32)).unwrap(),
            derivation: None,
        },
        supported_crypto_suites: vec![CryptoSuite::Secp256k1Keccak256Recoverable],
    }
}

#[test]
fn ac05_wrong_backend_keyref_or_algorithm_fails_closed() {
    let enrollment = enrolled();
    enrollment
        .authorize(
            &enrollment.key_ref,
            CryptoSuite::Secp256k1Keccak256Recoverable,
        )
        .unwrap();

    let mut wrong_backend = enrollment.key_ref.clone();
    wrong_backend.backend = Token::new("aws-kms").unwrap();
    assert_eq!(
        enrollment
            .authorize(&wrong_backend, CryptoSuite::Secp256k1Keccak256Recoverable)
            .unwrap_err()
            .code,
        ProtocolErrorCode::KeyrefMismatch
    );
    let mut wrong_key = enrollment.key_ref.clone();
    wrong_key.locator = "key-2".into();
    assert_eq!(
        enrollment
            .authorize(&wrong_key, CryptoSuite::Secp256k1Keccak256Recoverable)
            .unwrap_err()
            .code,
        ProtocolErrorCode::KeyrefMismatch
    );
    assert_eq!(
        enrollment
            .authorize(&enrollment.key_ref, CryptoSuite::Secp256k1Sha256Recoverable)
            .unwrap_err()
            .code,
        ProtocolErrorCode::SuiteNotAllowed
    );
}

#[test]
fn ac19_unsupported_versions_refuse_without_downgrade() {
    for version in [
        ProtocolVersion { major: 0, minor: 0 },
        ProtocolVersion { major: 2, minor: 0 },
        ProtocolVersion { major: 1, minor: 1 },
    ] {
        assert_eq!(
            version.validate().unwrap_err().code,
            ProtocolErrorCode::UnsupportedVersion
        );
    }
    ProtocolVersion::CURRENT.validate().unwrap();
}

#[test]
fn ac33_all_hierarchical_bounds_are_independent() {
    let mut oversized_prefix = ((FRAME_MAX_BYTES + 1) as u32).to_be_bytes().to_vec();
    oversized_prefix.extend_from_slice(b"{}");
    assert!(
        decode_frame::<serde_json::Value>(&oversized_prefix)
            .unwrap_err()
            .message
            .contains("1 MiB")
    );

    let single = SigningPayloads::Single {
        payload: Base64UrlBytes::from_bytes(&vec![0; SINGLE_PAYLOAD_MAX_BYTES + 1]),
    };
    assert!(single.validate().unwrap_err().message.contains("single"));
    assert_framed_decode_rejects::<SigningPayloads>(&single);

    let child = SigningPayloads::Batch {
        children: vec![Base64UrlBytes::from_bytes(&vec![
            0;
            BATCH_CHILD_MAX_BYTES + 1
        ])],
    };
    assert!(child.validate().unwrap_err().message.contains("child"));
    assert_framed_decode_rejects::<SigningPayloads>(&child);

    let aggregate_over_limit_with_frame_under_one_mib = SigningPayloads::Batch {
        children: vec![Base64UrlBytes::from_bytes(&vec![0; BATCH_CHILD_MAX_BYTES]); 9],
    };
    assert!(
        aggregate_over_limit_with_frame_under_one_mib
            .validate()
            .unwrap_err()
            .message
            .contains("aggregate")
    );
    assert_framed_decode_rejects::<SigningPayloads>(&aggregate_over_limit_with_frame_under_one_mib);

    let thirty_two_full_children = SigningPayloads::Batch {
        children: vec![
            Base64UrlBytes::from_bytes(&vec![0; BATCH_CHILD_MAX_BYTES]);
            BATCH_CHILD_MAX_COUNT
        ],
    };
    assert!(
        thirty_two_full_children
            .validate()
            .unwrap_err()
            .message
            .contains("aggregate")
    );

    let too_many = SigningPayloads::Batch {
        children: vec![Base64UrlBytes::from_bytes(&[]); BATCH_CHILD_MAX_COUNT + 1],
    };
    assert!(too_many.validate().unwrap_err().message.contains("1-32"));
    assert_framed_decode_rejects::<SigningPayloads>(&too_many);

    let hpke = HpkeEnvelope {
        kem_output: Base64UrlBytes::from_bytes(&[0; 32]),
        ciphertext: Base64UrlBytes::from_bytes(&vec![0; HPKE_ENVELOPE_MAX_BYTES]),
    };
    assert!(hpke.validate().unwrap_err().message.contains("HPKE"));
    assert_framed_decode_rejects::<HpkeEnvelope>(&hpke);

    let mut key = enrolled().key_ref;
    key.locator = "x".repeat(KEYREF_LOCATOR_MAX_BYTES + 1);
    assert!(key.validate().unwrap_err().message.contains("locator"));
    assert_framed_decode_rejects::<KeyRef>(&key);
}

fn assert_framed_decode_rejects<T: for<'de> Deserialize<'de>>(value: &impl Serialize) {
    let payload = serde_jcs::to_vec(value).unwrap();
    assert!(
        payload.len() <= FRAME_MAX_BYTES,
        "test vector must reach the nested bound before the frame bound"
    );
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    assert!(decode_frame::<T>(&frame).is_err());
}

#[test]
fn peer_errors_cannot_override_the_closed_contract() {
    let forged = serde_json::json!({
        "code": "SERVICE_UNAVAILABLE",
        "retry": "never",
        "durable_effect": "none",
        "message": "peer attempted to weaken reconciliation"
    });
    assert_framed_decode_rejects::<ProtocolError>(&forged);

    let unknown = serde_json::json!({
        "code": "PROVIDER_MAYBE_ACCEPTED",
        "retry": "never",
        "durable_effect": "none",
        "message": "unknown peer error"
    });
    assert_framed_decode_rejects::<ProtocolError>(&unknown);

    let canonical = ProtocolError::new(
        ProtocolErrorCode::ServiceUnavailable,
        "status reconciliation required",
    );
    let frame = encode_frame(&canonical).unwrap();
    assert_eq!(decode_frame::<ProtocolError>(&frame).unwrap(), canonical);
}

#[test]
fn authenticated_outer_method_must_match_typed_dispatch_method() {
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let body = BrokerSignerRequest::SignerReadiness(Empty {});
    let request_digest =
        Digest32::from_bytes(Sha256::digest(serde_jcs::to_vec(&body).unwrap()).into());
    let unsigned = UnsignedEnvelope {
        protocol: ProtocolVersion::CURRENT,
        schema: Token::new(RPC_ENVELOPE_SCHEMA_V1).unwrap(),
        kind: EnvelopeKind::Request,
        method: Token::new("sealed_approval.revoke_all").unwrap(),
        operation_id: OperationId::new("11".repeat(32)).unwrap(),
        request_digest,
        caller_service_id: Token::new("bloom-broker").unwrap(),
        caller_boot_epoch: BootEpoch::new("22".repeat(16)).unwrap(),
        audience: Token::new("bloom-signer").unwrap(),
        sent_at_ms: DecimalU64::new(10),
        deadline_ms: DecimalU64::new(20),
        body,
        application_key_id: Token::new("broker-app-1").unwrap(),
    };
    let envelope = SignedEnvelope {
        signature: Base64UrlBytes::from_bytes(
            &signing_key
                .sign(&unsigned.canonical_bytes().unwrap())
                .to_bytes(),
        ),
        unsigned,
    };
    let peer = AuthenticatedPeer {
        effective_uid: 501,
        service_id: Token::new("bloom-broker").unwrap(),
        boot_epoch: BootEpoch::new("22".repeat(16)).unwrap(),
        audience: Token::new("bloom-signer").unwrap(),
        application_key_id: Token::new("broker-app-1").unwrap(),
        application_public_key: signing_key.verifying_key().to_bytes(),
    };

    envelope.verify(501, &peer).unwrap();
    assert_eq!(
        envelope.verify_typed(501, &peer).unwrap_err().code,
        ProtocolErrorCode::MalformedFrame
    );
}

#[test]
fn typed_services_reject_unknown_states_and_fields_in_frames() {
    let unknown_state = serde_json::json!({
        "approval_id": "11".repeat(32),
        "wallet_id": "wallet-1",
        "state": "PAUSED",
        "effective_claim_assurance": null,
        "ceremony_url": null,
        "ceremony_expires_at_ms": null
    });
    assert_framed_decode_rejects::<ApprovalPublicStatus>(&unknown_state);

    let unknown_outer_field = serde_json::json!({
        "method": "broker.readiness",
        "body": {},
        "extension": true
    });
    assert_framed_decode_rejects::<MachineBrokerRequest>(&unknown_outer_field);

    let unknown_body_field = serde_json::json!({
        "method": "broker.readiness",
        "body": {"extension": true}
    });
    assert_framed_decode_rejects::<MachineBrokerRequest>(&unknown_body_field);
}

#[test]
fn unknown_fields_are_rejected_before_dispatch() {
    let value = serde_json::json!({"a":"ok", "extra":true, "z":7});
    let payload = serde_jcs::to_vec(&value).unwrap();
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Closed {
        #[allow(dead_code)]
        a: String,
        #[allow(dead_code)]
        z: u8,
    }
    let error = decode_frame::<Closed>(&frame).unwrap_err();
    assert_eq!(error.code, ProtocolErrorCode::UnknownField);
}
