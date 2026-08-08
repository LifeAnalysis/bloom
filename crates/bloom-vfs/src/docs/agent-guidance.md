# Working with bloom

bloom exposes Ethereum workflows as a virtual filesystem. Prefer inspecting the
mounted tree with normal filesystem tools. Run commands from the mount root
(the directory containing this file) so examples can use relative paths.

Useful commands:

- `ls` lists the VFS root.
- `ls docs` lists the embedded documentation.
- `cat docs/README.md` reads the VFS overview.
- `cat docs/examples.md` reads workflow examples.
- `cat docs/petals.md` discovers the Petals installed in this Bloom home.
- `ls chains` lists configured networks.

For more information, start in the `docs` folder. It contains the canonical
VFS usage notes and examples exposed by the mounted tree.

## Security model

bloom gatekeeps every value-moving action through capabilities:

- **Reads are always safe.** No signing, no ceremony, no wallet needed for chain
  state, balances, prices, books, candles, account state.
- **Direct writes require owner approval.** The outbox stage-confirm flow and
  one-off actions through the default Hyperliquid and Polymarket Petals each
  cross an owner gate (passkey ceremony or local passphrase unlock).
- **Automated action uses a capability.** Create a bounded session/capability
  first — the human approves the bounds once, then the agent operates inside
  the checks implemented by that extension. Inspect the extension's lifecycle,
  expiry, cleanup, and revocation semantics before relying on it.
- **The owner key is never handed off.** For capabilities that depend on owner
  signing (EVM and installed Petals), the key will
  reside in daemon RAM for a bounded window and auto-lock on expiry.
  The Hyperliquid Petal uses an ephemeral agent key that does not need the
  owner key after session creation.

To see what a wallet can do without a human, check its per-chain state and
outbox. A read-only `wallets/<wallet>/capabilities/` roll-up and a VFS-root
`next.md` aggregator expose Bloom-native capability and next-action state.
Installed Petals expose their own durable session and status files.

For the default-installed Hyperliquid Petal, start with
`cat petals/hyperliquid/README.md` and list its route tree before trading.
Read `docs/petals.md` for every installed Petal's mount directory, consent
summary, and declared capabilities.
For the default-installed Polymarket Petal, start with
`cat petals/polymarket/README.md`, read `petals/polymarket/AGENTS.md`, then
inspect `petals/polymarket/meta/route-contract.json` and list its route tree
before using its prediction-market routes.

## Wallets

When asked for an address for a certain wallet, consider displaying in-line the QR code image for the relevant wallet e.g. `wallets/<wallet>/address.qr.png`.

## Creating a wallet (asynchronous passkey registration)

Writing a plain name to `wallets/new` **starts a passkey registration — it
does not create a local wallet**, and the write returns before the ceremony
completes:

```sh
printf 'main\n' > wallets/new
cat wallets/registrations/main/status.json
cat wallets/registrations/main/ceremony_url
```

- Read `status.json` and `ceremony_url` right after the write.
- Open or forward `ceremony_url` to a human; do not attempt it yourself. Never
  imitate WebAuthn, supply PRF material, read recovery material, or silently
  downgrade to a passphrase wallet — none of that is available or safe from
  an agent.
- If the registration page reports an unsupported passkey method, ask the
  human to retry with a browser/device passkey, a password-manager passkey
  (iCloud Keychain, Google Password Manager), or a compatible hardware
  security key — and specifically to choose **"Use browser, device, or
  hardware key"** if Bitwarden intercepts the prompt.
- Do not proceed until `status.json`'s `state` is `completed`; only then read
  the new wallet's address at `wallets/<name>/address`.
- Asynchronous passkey registration requires a running `bloom serve` daemon.
  If no registration service is available, the write fails clearly and tells
  you to start `bloom serve`; retry after it is running rather than falling
  back to any other wallet-creation path.
- To cancel a live registration, write anything to
  `wallets/registrations/<name>/cancel`.

## Importing an existing private key (passkey ceremony)

Importing a key uses the same asynchronous ceremony as creating a wallet, but
stages through `wallets/import/<name>` so the **private key never enters the
VFS, the shell, or the daemon socket** — the human pastes it directly into the
browser ceremony page, which encrypts it under a new passkey:

```sh
printf 'main\n' > wallets/import/main
cat wallets/registrations/main/status.json
cat wallets/registrations/main/ceremony_url   # forward to the human
```

- The write to `wallets/import/<name>` carries **only the name** (any non-empty
  body; never a key). The body MUST NOT contain a key or TOML — the handler
  refuses it.
- Forward `ceremony_url` to the human. The page shows a private-key field; the
  human pastes the key and completes WebAuthn. The key is wrapped under the
  passkey server-side and never persists as plaintext or a passphrase.
- The ceremony projection is shared with new-wallet registration
  (`wallets/registrations/<name>/`), so poll `status.json` until `completed`,
  then read `wallets/<name>/address`. The recovery key shown at the end IS the
  imported private key (passkey-wrapped wallets recover to their raw key).
- Writing `kind = "passkey-import"` to `wallets/new` is rejected with a pointer
  to `wallets/import/<name>` — use the dedicated path.

## Local / passphrase wallets (synchronous, non-ceremony)

Explicit `kind = "local"` and `kind = "import"` wallets remain synchronous and
require `allow_passphrase_wallet = true` plus a passphrase in the TOML spec —
passkey is the default for a reason. These do not use a ceremony; the key (for
`import`) must be supplied in the TOML, which is why the passkey import path
above is strongly preferred for any real key.

## Mounted Sealed Approval flow

When working through a mounted tree, a confirm write that needs fresh owner
approval does **not** open a browser by itself. The daemon exposes the challenge
first, then denies the triggering write so the writing agent can deliberately
open or forward the ceremony URL.

Expected mounted flow for value-moving outbox actions:

```sh
# 1. Stage a Petal action, then discover its concrete central action id.
ls outbox/pending
cat outbox/pending/<action_id>/plan.md

# 2. Confirm through the Petal projection. If fresh approval is needed, opening
#    the write should fail with permission denied after the daemon writes
#    approval_challenge.json.
printf 'confirm\n' > wallets/<wallet>/chains/<chain>/outbox/pending/<id>/confirm

# 3. Read the challenge from the same central action directory.
cat outbox/pending/<action_id>/approval_challenge.json
```

Before opening the ceremony, verify that `approval_challenge.json` has the same
`action_id` as the directory you are acting on and that `expiry_ms` is still in
the future. Then open or forward `ceremony_url`.

The ceremony page offers two modes:

- **grant**: mints only an in-memory daemon grant. Retry the same mounted
  confirm write to execute from the sealed bytes.
- **grant + execute**: mints the grant and executes immediately in the daemon.

If the pending transaction has soft policy warnings and you intend to bypass
them, use the sibling write sink `confirm.override`. Mounted override intent
lives in the path so Bloom can make the approval decision before accepting
payload bytes.

After execution, inspect `outbox/sent/<action_id>/` or
`outbox/failed/<action_id>/` for `status.json`, `result.json`, and audit/result
artifacts. Petal-specific wallet paths are projections of the same central
action id; do not treat them as separate approval queues.

## Editing a passkey wallet policy

For a passkey (WebAuthn-gated) wallet, `policy.toml` is signed authorization
state (`policy.toml.sig`). Editing it through the mount is a Sealed Approval
action — the daemon installs both the new `policy.toml` and its matching
signature only after owner approval. Local (passphrase) wallets keep their
old behavior: the write applies immediately.

```sh
# 1. Read the current signed policy and edit it locally.
cat wallets/<wallet>/policy.toml

# 2. Write the proposed policy. For a passkey wallet the first write fails with
#    permission denied after the daemon stages a Sealed Approval challenge.
printf '%s' "$edited_policy" > wallets/<wallet>/policy.toml

# 3. Discover and read the challenge through the mount.
ls wallets/<wallet>/policy-updates/pending
cat wallets/<wallet>/policy-updates/latest/status.json
cat wallets/<wallet>/policy-updates/latest/approval_challenge.json

# 4. Open or forward ceremony_url, approve, then retry the identical write.
printf '%s' "$edited_policy" > wallets/<wallet>/policy.toml
```

The retry must send the **same** proposed bytes: the action id (and therefore
the grant) is bound to `blake3(old_policy)` and `blake3(proposed_policy)`.
Different retry bytes re-derive a fresh action id and start a new challenge
rather than reusing the prior approval. On the approved retry Bloom also
re-checks that the current on-disk policy still matches the sealed baseline,
signs the approved proposed policy through the host signer, writes
`policy.toml.sig`, then installs `policy.toml` — so the wallet is never left with
a new policy that lacks its matching signature.

`status.json` and `approval_challenge.json` are read-only views: they carry
bounded challenge metadata and `ceremony_url` only, never the signed approval or
any key/PRF/grant material.

## Paid HTTP (x402 and MPP)

Paid HTTP requests live under `requests`. Agents should stage the request,
read `plan.md`, and confirm only when the quoted cost, network, asset, and
merchant match the task. Bloom handles both x402 and MPP internally.

### Selecting the payment protocol

Bloom handles credentials and settlement internally, but the merchant may use
request headers to select its payment rail.

For a merchant supporting both x402 and MPP:

- No negotiation header may default to x402.
- To request MPP, include the negotiation header
  `authorization: Payment` in the staged request.

### Approval

If paid confirmation needs passkey approval, the first confirm write may return
permission denied after writing
`requests/pending/<id>/approval_challenge.json`. Read that file, check
`action_id`, `expiry_ms`, merchant/payment details in `plan.md`, then open or
forward `ceremony_url`, then retry the same VFS write.

The ceremony mints a short-lived in-memory grant for the sealed request. x402
and MPP then ask Bloom's host signer to sign the exact payment digest under that
grant; one allowance is consumed atomically only when a signature is produced.
Failed policy checks, bad attestations, failed credential preparation, or retry
failures before signing do not consume the grant. Raw payment authorization
headers, signed payloads, passkey material, and PRF output are not written to
VFS artifacts; credential metadata is redacted.

Request confirmation executes from the daemon's sealed paid-HTTP subject bytes
and sealed policy snapshot. Files such as `request.toml`, `challenge.json`, and
`policy_check.json` are views for agents. If a pending projection differs from
the sealed subject, or `private/request_body` no longer matches the sealed body
hash, confirmation fails before signing or minting credentials. Live policy may
narrow or deny, but it cannot widen the already sealed payment terms.

### Examples

```sh
cat > requests/new <<'REQUEST'
POST https://api.exa.ai/search wallet=<wallet> max_amount_usd=0.05
content-type: application/json

{"query":"latest Base USDC x402 developer tools","numResults":5,"type":"auto"}
REQUEST

cat requests/latest/plan.md
printf 'confirm\n' > requests/latest/confirm
cat requests/latest/response/body
cat requests/latest/receipt.json
```

Wallet-native paid endpoints that passed live checks:

```sh
# Pre-swap token safety for Base USDC + WETH.
printf '%s\n' 'GET https://x402.fiasignals.com/token-safety/batch?chain=base&token_addresses=0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913,0x4200000000000000000000000000000000000006 wallet=<wallet> max_amount_usd=0.05' > requests/new

# Hyperliquid trader score.
cat > requests/new <<'REQUEST'
POST https://graphadvocate.com/hyperliquid/score wallet=<wallet> max_amount_usd=0.03
content-type: application/json

{"user":"0x..."}
REQUEST

# MPP forced based on presence of `authorization: Payment` header
cat > requests/new <<'REQUEST'
POST https://api.nansen.ai/api/v1/smart-money/netflow wallet=<wallet>max_amount_usd=0.10
content-type: application/json
accept: application/json
authorization: Payment

{"chains":["ethereum"],"filters":{"token_sectors":["DeFi"],"include_stablecoins":false},"pagination":{"page":1,"per_page":10},"order_by":[{"field":"net_flow_7d_usd","direction":"ASC"}]}
REQUEST


# Polymarket ghost-fill risk.
cat > requests/new <<'REQUEST'
POST https://graphadvocate.com/polymarket/risk wallet=<wallet> max_amount_usd=0.03
content-type: application/json

{"wallet":"0x..."}
REQUEST

# Onchain data routing to GraphQL or REST.
cat > requests/new <<'REQUEST'
POST https://graphadvocate.com/route wallet=<wallet> max_amount_usd=0.02
content-type: application/json

{"request":"Find the best subgraph for Uniswap V3 pools on Base and give me a ready GraphQL query for top pools by TVL"}
REQUEST
```

Prefer request-local USD caps. If `plan.md` says policy is denied, do not retry
blindly; inspect the wallet policy or ask the human to change it.

## Hyperliquid

Hyperliquid trading uses Sealed Approval for owner authority:

- **Read the installed contract first.** Inspect
  `petals/hyperliquid/README.md`, `AGENTS.md`, and `ASSET_IDS.md`; request bodies
  and supported cleanup routes are version-specific.
- **Agent sessions are advanced delegated authority.** Set `assets`,
  `max_notional_usd`, and `max_leverage` explicitly. In v0.1.4 those checks run
  per route write; there is no background loss/position monitor or automatic
  flattening. Expiry or `stop` blocks later writes but does not cancel orders,
  close positions, or revoke venue authority.
- **Owner actions:** use the exact tagged action schemas documented by the
  installed Petal. An accepted write is not evidence of a fill; inspect its
  durable response and error files.

## Petals

Petals are installed wallet extensions. They add application-specific routes
under `petals/<name>/` while using Bloom's wallet, policy, approval, network,
storage, and transaction capabilities.

Installed Petals vary by Bloom home; do not assume a particular extension is
present. Read `docs/petals.md` first. It is generated from the immutable
`petal.toml` manifests of the currently installed packages and lists:

- the installed Petal name and its `petals/<name>/` directory;
- the package's `[consent].summary`; and
- the capabilities declared by that package.

After choosing a Petal, read `petals/<name>/README.md` and
`petals/<name>/AGENTS.md`, then list its directory to discover the current
route tree. Treat those package documents as authoritative for its workflows
and review requirements.

## Passkey policy mode

For passkey wallets, policy writes through `wallets/<wallet>/policy.toml` may
produce a mounted approval challenge. The review page lets the human choose:

- `Ask me every time`: money-moving actions need a fresh passkey review.
- `Let Bloom use these rules`: agents may proceed only when every signed policy
  check passes.

Do not treat `under_policy` as a blanket unlock. It is permission to act inside
the wallet's signed caps, allowlists, and surface-specific rules.
