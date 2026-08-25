# Test fixture contract

This package exists only for the out-of-process triad integration harness. It
must not be preinstalled, copied into a release bundle, or treated as a
production Petal. Its mounted interface must remain ordinary filesystem I/O.

The guest may receive public key metadata (`KeyRef`, addresses), operation and
ceremony identifiers, approval metadata, and signatures. It must never receive
private key bytes or invoke a hash-only signing interface.
