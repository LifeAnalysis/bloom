# VFS/CLI Gap Plan

**Status:** superseded historical plan
**Original date:** 2026-06-28
**Superseded by:** the triad architecture and Machine legacy-authority removal

This file previously contained an implementation plan for the pre-triad
single-process Bloom daemon. It is retained only as a tombstone so old links do
not present that plan as current developer guidance. Its foreground wallet
unlock, embedded ceremony, local signing host, native venue handler, and
Machine-owned delegated-key assumptions are retired and must not be restored.

Do not use the former plan to implement product behavior or parity tests.
Current requirements are:

- Machine is key-free and uses Broker as its only authority service;
- Broker owns Sealed Approval, review, policy evaluation, ceremony HTTP, and
  authorization budgets;
- Signer owns wallet and Petal-scoped delegated keys and produces every
  wallet-controlled signature;
- CLI, foreground VFS, and mounted VFS use the same Broker/Signer authority
  path;
- payload-bearing Petal signing is mandatory and hash-only signing fails
  closed;
- policy update uses `policy.validate_update`, the shared custody ceremony,
  and `policy.commit_update` with a completed receipt; and
- venue integrations such as Polymarket and Hyperliquid are external Petals,
  not native Machine authority surfaces.

Use these current documents instead:

- [`2026-07-23-triad-process-architecture.md`](../specs/2026-07-23-triad-process-architecture.md)
- [`2026-07-31-machine-legacy-authority-removal.md`](../specs/2026-07-31-machine-legacy-authority-removal.md)
- [`Interaction Modes.md`](../architecture/Interaction%20Modes.md)
- [`Sealed Approvals.md`](../architecture/Sealed%20Approvals.md)
- [`local-mainnet-integration.md`](../local-mainnet-integration.md)

New parity work should be specified against installed Petal route contracts,
Machine public projections, Broker operation IDs, and Signer receipts. It must
not infer support from paths or commands recorded in the deleted historical
body of this plan.
