//! Command cores shared by the CLI binary and tests.
//!
//! Everything derives from durable records — the staged view and the request
//! are rebuilt from the envelope plus journal, never from memory or display
//! files. A confirm never rebuilds the transaction; it re-derives the staged
//! facts and runs the finalize gate (verifier re-run, fee re-observation,
//! freshness, exact approval, sign).

use bloom_chain_action::Action;
use bloom_solana_machine::{AccountRegistry, SolanaMachine};

/// Rebuild the staged view (message, digests, blockhash, fee) from durable
/// journal records alone.
pub fn staged_from_action(action: &Action) -> bloom_solana_machine::StagedTransfer {
    let env = &action.envelope;
    let mut blockhash = String::new();
    let mut last_valid = 0u64;
    let mut fee = 0u64;
    for record in &action.journal {
        match &record.transition {
            bloom_chain_action::Transition::LivenessObserved {
                reference,
                valid_until,
                ..
            } => {
                blockhash = reference.clone();
                last_valid = match valid_until {
                    bloom_chain_action::ValidUntil::Height { value }
                    | bloom_chain_action::ValidUntil::Slot { value }
                    | bloom_chain_action::ValidUntil::TimeMs { value } => *value,
                };
            }
            bloom_chain_action::Transition::FeeObserved { fee: f, .. } => {
                fee = f.amount.parse().unwrap_or(0)
            }
            _ => {}
        }
    }
    bloom_solana_machine::StagedTransfer {
        operation_id: env.operation_id.clone(),
        message_hex: env.payload_hex.clone(),
        payload_digest_hex: env.payload_digest_hex.clone(),
        blockhash_base58: blockhash,
        last_valid_block_height: last_valid,
        fee_lamports: fee,
    }
}

/// Rebuild the transfer request from durable records + the account registry.
pub fn request_from_action(
    accounts: &AccountRegistry,
    action: &Action,
) -> anyhow::Result<bloom_solana_machine::TransferRequest> {
    let env = &action.envelope;
    let account = accounts.get(&env.wallet_id, &env.chain.profile)?;
    let facts = action
        .journal
        .iter()
        .rev()
        .find_map(|r| match &r.transition {
            bloom_chain_action::Transition::FactsVerified { core, .. } => {
                let transfer = core.transfers.first()?;
                Some((
                    core.signer_account.rsplit(':').next()?.to_string(),
                    transfer.to.rsplit(':').next()?.to_string(),
                    transfer.amount.parse().ok()?,
                ))
            }
            _ => None,
        });
    let Some((fee_payer_base58, destination, lamports)) = facts else {
        anyhow::bail!(
            "operation {} has no verified-facts record",
            env.operation_id
        );
    };
    let max_fee = action
        .journal
        .iter()
        .rev()
        .find_map(|r| match &r.transition {
            bloom_chain_action::Transition::FeeObserved { ceiling, .. } => {
                ceiling.amount.parse().ok()
            }
            _ => None,
        })
        .unwrap_or(100_000);
    Ok(bloom_solana_machine::TransferRequest {
        operation_id: env.operation_id.clone(),
        wallet_id: env.wallet_id.clone(),
        fee_payer_base58,
        destination_base58: destination,
        lamports,
        key_ref: bloom_solana::adapter::FixtureKeyRef {
            backend: "local".into(),
            locator: account.key_ref_locator.clone(),
            public_key_hex: account.public_key_hex.clone(),
        },
        expires_at_ms: env.expires_at_ms,
        max_fee_lamports: max_fee,
        claimed_caip2: env.chain.claimed_caip2.clone(),
    })
}

/// Confirm: rebuild from durable records, run the finalize gate, sign once.
/// The referenced operation must exist; nothing is ever rebuilt from user
/// input alone.
pub async fn confirm_operation(
    machine: &SolanaMachine,
    accounts: &AccountRegistry,
    operation_id: &str,
) -> anyhow::Result<()> {
    let action = machine.load_action(operation_id);
    let staged = staged_from_action(&action);
    let request = request_from_action(accounts, &action)?;
    // The signer identity must still match the registered account key.
    let named = hex::decode(&request.key_ref.public_key_hex)?;
    if named != machine_signer_key(machine) {
        anyhow::bail!("registered account key no longer matches the signing authority");
    }
    let now = crate::session::system_now_ms();
    machine.finalize_transfer(&request, &staged, now).await?;
    Ok(())
}

fn machine_signer_key(machine: &SolanaMachine) -> Vec<u8> {
    machine.signer_public_key().to_vec()
}
