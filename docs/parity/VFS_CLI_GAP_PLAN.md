# VFS/CLI Gap Plan

Date: 2026-06-28

This plan ranks the gaps identified in
`docs/parity/VFS_CLI_PARITY_LEDGER.md`. The ranking favors agent/product value
first, then implementation risk. Value-moving workflows must use shared
execution logic; VFS handlers may parse path/body input, but must not duplicate
signing, posting, policy, geoblock, lock, receipt, or broadcast logic.

## 0. Implemented: Polymarket fund request confirm via foreground VFS CLI

Goal: execute a funding request staged through `/polymarket/fund/<wallet>/new`
through a VFS-shaped confirm path.

User story: an agent stages a pUSD funding request, reads the durable plan, then
asks the owner to run one foreground VFS write that executes that exact request.

Exact CLI behavior matched:

```bash
bloom polymarket fund <wallet> --request <id> [--dry-run] [--confirm-risk]
```

Exact VFS path and body:

```bash
bloom vfs write /polymarket/fund/<wallet>/<id>/confirm \
  --unlock-wallet <wallet> \
  --data confirm

bloom vfs write /polymarket/fund/<wallet>/<id>/confirm \
  --unlock-wallet <wallet> \
  --data '{"confirm":true,"dry_run":true,"confirm_risk":true}'
```

Shared core function to call:

- `commands::polymarket::fund_from_request`, which calls
  `commands::polymarket::fund`, `TxEngine::stage`, and `TxEngine::confirm`.

Safety invariants:

- request id rejects traversal;
- unlock wallet must match path wallet;
- request body must affirm confirmation;
- fund core re-reads live pUSD balance and live route quotes;
- fund core enforces onboarding/deposit-wallet owner binding;
- route policy, EVM policy, outbox review, passkey/local unlock, broadcast, and
  request executed marking stay in the shared CLI funding path;
- mounted/IPC handler advertises the path for discovery but refuses execution
  with foreground CLI guidance, because the signer ceremony must live in the
  signing process.

Tests added:

- CLI parser tests for ack body, structured JSON body, wallet mismatch, and
  unrelated path ignoring;
- VFS handler test proving a staged fund request exposes `confirm`, renders
  guidance, and refuses direct handler execution with foreground-unlock text.

Docs updated:

- Polymarket VFS README string;
- `docs/polymarket-integration.md`;
- `EXAMPLES.md`;
- `QUICKSTART.md`.

Rollback/non-goals:

- rollback by removing the CLI intercept and VFS `confirm` discoverability;
- does not implement mounted daemon signing;
- does not implement Polymarket trade draft confirm.

## 1. Implemented: Polymarket trade draft confirm/post via foreground VFS CLI

Goal: add a writable confirmation path for order and sell-to-close drafts.

User story: an agent creates a draft through
`/polymarket/trade/<wallet>/new`, reviews `drafts/<id>/plan.md`, and asks the
owner to confirm by writing to a VFS path rather than switching to a separate
`polymarket confirm` command.

Exact CLI behavior to match:

```bash
bloom polymarket confirm <wallet> <draft-id> [--confirm-risk]
```

Exact proposed VFS path and body:

```bash
bloom vfs write /polymarket/trade/<wallet>/drafts/<id>/confirm \
  --unlock-wallet <wallet> \
  --data confirm

bloom vfs write /polymarket/trade/<wallet>/drafts/<id>/confirm \
  --unlock-wallet <wallet> \
  --data '{"confirm":true,"confirm_risk":true}'
```

Shared core function called:

- `commands::polymarket::confirm`, which loads the durable draft and calls the
  existing internal `execute` path used by `bloom polymarket confirm`.
- VFS path/body parsing stays in the command layer; the mounted VFS handler only
  exposes discovery/help and refuses direct execution with foreground CLI
  guidance.

Safety invariants preserved:

- stale draft refusal;
- geoblock behavior identical to CLI;
- policy re-check from current policy;
- order lock around confirm/post/receipt writes;
- sell-to-close holdings preflight;
- passkey/local unlock with exact Polymarket order review intent;
- CLOB post rejection/ambiguous reconciliation behavior unchanged;
- receipt/audit artifacts identical to CLI.

Tests added:

- parser/routing tests for confirm path and body;
- mounted VFS handler discovery/read/refusal test for `drafts/<id>/confirm`;
- subprocess parity smoke test proving `bloom polymarket confirm` and
  `bloom vfs write /polymarket/trade/<wallet>/drafts/<id>/confirm
  --unlock-wallet <wallet> --data confirm` share the same durable missing-draft
  refusal before network/signing work.

Docs updated:

- Polymarket VFS README string;
- `docs/parity/VFS_CLI_PARITY_LEDGER.md`;
- `docs/polymarket-integration.md`;
- `EXAMPLES.md`;
- `QUICKSTART.md`;
- `README.md`.

Rollback/non-goals:

- rollback by removing the VFS dispatch and keeping CLI confirm only;
- do not add a second order signer/poster;
- do not weaken geoblock, stale-draft, policy, lock, or receipt guarantees.
- mounted/IPC handler execution remains intentionally unsupported because the
  signer ceremony must live in the foreground process.

## 2. Implemented: Polymarket risk-reducing VFS actions

Goal: expose cancel, redeem, revoke approvals, and pUSD withdraw through VFS
action paths. Cancel executes directly (no signing); the three owner-signed
actions follow the foreground-confirm pattern.

User story: an agent can discover and execute operational safety actions from
the same `/polymarket` namespace used for positions and account state.

Exact CLI behavior to match:

```bash
bloom polymarket cancel <wallet> <order-id>
bloom polymarket redeem <wallet> <slug> [--dry-run]
bloom polymarket revoke-approvals <wallet> [--dry-run]
bloom polymarket withdraw-pusd <wallet> <amount|all> [--dry-run]
```

Exact proposed VFS paths and bodies:

```text
/polymarket/trade/<wallet>/orders/<order-id>/cancel              # direct handler exec (no unlock)
/polymarket/redeem/<wallet>/<slug>/{plan.md,confirm}             # foreground confirm
/polymarket/revoke-approvals/<wallet>/request/{plan.md,confirm}  # foreground confirm
/polymarket/withdraw/<wallet>/pusd/{plan.md,confirm}             # foreground confirm
```

Path convention keeps `<wallet>` immediately after `/polymarket` and an id slot
throughout, matching fund (`/polymarket/fund/<wallet>/<id>/...`) and trade
(`/polymarket/trade/<wallet>/drafts/<id>/...`). Resting CLOB orders live under
the trade namespace. `revoke-approvals` and `withdraw` are singleton actions, so
their id slot is the literal `request` / `pusd` segment.

Body: `confirm`, `y`, or JSON/TOML with `confirm=true` and optional
`confirm_risk`. `dry_run` is **rejected on `/confirm`** (confirm is execute);
the dry-run representation is the read-side `plan.md`. `cancel` accepts only the
ack body (`confirm`/`y`/`yes`); it takes no `--unlock-wallet` and no risk fields.

Execution shapes:

- `cancel` runs **directly in the mounted VFS handler** (like `new` paths) because
  it uses stored CLOB credentials and performs no owner signing.
- `redeem`, `revoke-approvals`, and `withdraw-pusd` follow the **foreground
  confirm** pattern established by fund/trade-confirm: the mounted handler
  advertises the path and renders guidance but refuses direct execution, because
  the signer ceremony must live in the foreground process.

Shared core function to call:

- Extract reusable service functions (`redeem_service`, `revoke_approvals_service`,
  `withdraw_pusd_service`, `cancel_service`) from the existing CLI
  implementations; CLI handlers delegate to them.
- Keep `submit_and_confirm_wallet_batch` as the shared relayer batch helper.
- The post-confirm on-chain verification loop in `revoke_approvals` (re-reading
  allowances and `isApprovedForAll` after the batch lands) must stay **inside**
  `revoke_approvals_service` so CLI and VFS cannot diverge on the safety check.

Safety invariants:

- cancel remains risk-reducing and geoblock warning-only;
- redeem refuses before Data API marks a position redeemable unless dry-run;
- revoke verifies allowances/operators are zero after confirmation;
- withdraw checks deposit-wallet pUSD balance;
- all owner-signed relayer operations use passkey/local unlock and order lock.

Tests added:

- CLI parser tests for redeem/revoke-approvals/withdraw-pusd confirm bodies (ack,
  JSON, TOML, wallet mismatch, unconfirmed, ignore-other; withdraw also rejects
  bare ack and missing amount);
- VFS handler test proving all three owner-signed surfaces advertise `confirm`,
  render guidance, and refuse direct handler execution; and that cancel
  advertises, renders guidance, and executes in-handler (failing on a durable
  pre-network gate rather than refusing);
- CLI subprocess parity smoke tests proving `bloom polymarket
  redeem|revoke-approvals|withdraw-pusd` and the matching
  `bloom vfs write .../confirm --unlock-wallet` paths share the same durable
  refusal (missing wallet) before any network/signing work; plus a
  test that withdraw confirm rejects a bare ack.

Docs updated:

- `docs/polymarket-integration.md`;
- `/polymarket/README.md`;
- `EXAMPLES.md`;
- `QUICKSTART.md`;
- `docs/parity/VFS_CLI_PARITY_LEDGER.md` (cancel/redeem/revoke/withdraw rows
  flipped to `parity`).

Rollback/non-goals:

- do not implement these as raw tx writes;
- do not add VFS paths until shared execution functions exist.

## 3. Implemented: Wallet outbox replace/cancel parity

Goal: make pending transaction cancellation/replacement explicit across CLI and
VFS.

User story: after staging a transaction, an agent can cancel or replace it
without manually editing outbox files.

Exact CLI behavior matched:

```bash
bloom wallet cancel <wallet> <chain> <id> [--text y] [--passphrase <passphrase>]
bloom wallet replace <wallet> <chain> <id> --intent '<replacement-intent>' [--passphrase <passphrase>]
```

`replace` reads the replacement intent from stdin when `--intent` is omitted.

Exact VFS path and body:

```text
/wallets/<wallet>/chains/<chain>/outbox/pending/<id>/cancel
/wallets/<wallet>/chains/<chain>/outbox/pending/<id>/replace
```

Shared core function called:

- the dedicated CLI commands dispatch through `write_unlocked` to the same VFS
  paths;
- `WalletsHandler::write_outbox` calls `TxEngine::cancel` and
  `TxEngine::replace_with_intent`;
- IPC/mount classify `cancel` and `replace` as wallet-signer writes, so the
  signer ceremony stays in the foreground/unlocked path.

Safety invariants:

- only pending, unbroadcast entries can be cancelled directly;
- replacement must preserve nonce safety and policy gates;
- no unrelated outbox entries are modified.

Tests added / existing coverage:

- CLI help smoke for first-class `wallet cancel` / `wallet replace`;
- existing `TxEngine` tests cover same-nonce replacement/cancel broadcast
  attempts, broadcast gates, policy gates, and marker artefacts;
- existing VFS tests cover `confirm`/`replace`/`cancel` control-file
  discoverability and write semantics.

Docs updated:

- parity ledger row flipped to `parity`.

Rollback/non-goals:

- do not expose filesystem deletes as the control plane.
- `confirm` body `cancel` remains local cancellation of an unbroadcast pending
  entry; the explicit `cancel` file/command submits a same-nonce network
  cancellation.

## 4. Audited: Hyperliquid native surface removed (moved to Petal)

Bloom no longer ships a native Hyperliquid handler, CLI subcommand, or
`bloom-hyperliquid` crate. All Hyperliquid reads, exchange writes, agent
sessions, and USD transfers moved to the standalone `bloom-petal-hyperliquid`
package. After install, the surface appears under the canonical Petal
namespace:

```text
/petals/hyperliquid/<network>/...
```

Fresh default homes provision the pinned external release during `bloom init`;
an explicit `[petals] preinstalled = []` remains the persistent opt-out.

Parity classification: `petal`, not tracked in this ledger. The external
Petal repository owns its own CLI, VFS, and test parity matrix. Bloom's
built-in DeFi handler retains only the Hyperliquid deposit-route bridge
address and deposit chain id (Arbitrum) from `[hyperliquid]` config.

## 4.2. Implemented: Polymarket builder-key list/revoke VFS parity

Goal: expose builder API key inspection and revocation through the Polymarket
VFS without exposing builder secrets.

Exact CLI behavior matched:

```bash
bloom polymarket builder-keys list <wallet>
bloom polymarket builder-keys revoke <wallet> [key]
```

Exact VFS path and body:

```text
/polymarket/builder-keys/<wallet>/keys.json
/polymarket/builder-keys/<wallet>/revoke
```

`revoke` accepts `confirm`, `y`, or JSON/TOML with `confirm=true` and optional
`key = "<builder-key-id>"`.

Shared core behavior:

- CLI and VFS both use stored CLOB credentials and
  `ClobClient::{list_builder_api_keys,revoke_builder_api_key}`;
- both delete local `builder_creds.json` when the revoked key matches the stored
  Bloom builder key;
- VFS `keys.json` returns key IDs/status metadata only, never secret or
  passphrase material.

Safety invariants:

- builder keys are relayer submission auth only and cannot move funds;
- revoke requires an explicit confirmation body;
- key IDs reject path traversal characters;
- no wallet owner signature is required, so revoke may execute inside the
  mounted handler like CLOB order cancel.

Tests added:

- VFS body parser tests for bare confirm, JSON/TOML keyed revoke, unconfirmed
  body rejection, and unsafe key-id rejection.

Rollback/non-goals:

- do not expose builder secrets/passphrases through VFS;
- do not create builder keys through VFS outside onboarding.

## 5. Audited: Petal, chain, and pipe execution split

Goal: make the product split explicit so petal install/run, chain admin/submit,
and pipe expression workflows are not mistaken for unresolved Track C parity
gaps.

User story: an agent can discover which petal/chain/pipe workflows are safe
benchmark candidates without inferring parity from adjacent read paths.

CLI surfaces:

```bash
bloom petals install|run|ls|name|uninstall ...
bloom chain init|run-validator|submit|query|call|pipe ...
bloom pipe <expr> --signer <hex> --gas-payer <hex>
```

VFS surfaces:

```text
/petals/... discovery/read endpoints
/petals/.pipe                         # executable shim: exec bloom chain pipe "$@"
/tx/new
/tx/<id>/cmd
/tx/<id>/signer
/tx/<id>/gas-payer
/tx/<id>/status
/tx/<id>/commit
/tx/<id>/abort
```

Classification:

- petal install/name/uninstall are `cli_only` / `track_b`: mutating local plugin
  management remains CLI-oriented;
- petal run is `hybrid_required` / `track_b`: VFS exposes endpoint discovery and
  chain petal surfaces, while broad local execution parity is intentionally
  deferred;
- chain node admin and raw submit are `cli_only` / `exclude`: validator
  lifecycle, xDSA keys, config, and raw transaction submission stay with local
  operator CLI commands;
- chain reads are VFS-compatible and remain `track_b` until an exact read matrix
  is selected;
- pipe/PTB execution is `parity` for the staged PTB substrate:
  `bloom pipe::lower_and_build` and VFS `TxHandler` both drive
  `bloom_ptb_builder::PtbSession`, and daemon-mounted `tx/<id>/commit` uses the
  injected `PtbSubmitter` for gas selection/sign/submit. The expression-language
  parser itself remains a CLI frontend and does not need a duplicate VFS parser.

Shared core already in use:

- `bloom_ptb_builder::PtbSession` for command append, validation, unsigned PTB
  build, status, and receipt projection;
- `bloom pipe::lower_and_build` lowers expression syntax to the same command
  lines a VFS client writes into `tx/<id>/cmd`;
- `TxHandler::commit_ndjson` renders the same canonical PTB/command NDJSON and
  appends submitter receipt lines when mounted in the daemon.

Safety invariants:

- no raw secret upload through VFS;
- no validator admin action through a mounted namespace without explicit local
  operator intent;
- PTB signing/gas-payer checks unchanged.

Tests to keep/add:

- existing pipe lower/build tests;
- existing tx handler session tests;
- add a golden test only when the benchmark suite is assembled: lower a pipe
  expression to command lines, stage those lines through `tx/<id>/cmd`, and
  assert the unsigned PTB digest plus command receipt projection match.

Docs to update:

- parity ledger and benchmark plans.

Rollback/non-goals:

- do not add a VFS expression parser unless product direction changes;
- do not expose validator admin or raw submit through mounted VFS paths.
