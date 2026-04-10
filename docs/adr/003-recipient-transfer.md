# ADR 003: Recipient Transfer Without Sender Veto

**Status:** Accepted  
**Date:** 2026-05-12

## Context

Stream recipients may want to reassign their earnings to a different address — for example, when rotating a wallet key, transferring a vesting grant to a new entity, or programmatically routing payments through a contract. The question is whether the sender should have veto power over such transfers.

## Decision

The `transfer_recipient` function requires only the **current recipient's signature**, not the sender's. The new recipient address is set immediately; no time-lock or pending state is introduced.

## Rationale

1. **Sender's exposure is unchanged.** The sender's obligation is to pay at the agreed rate until `end_time`. Who that payment goes to is the recipient's concern, not the sender's. From the sender's perspective, tokens leave at the same rate regardless.

2. **Complexity of a veto model.** A veto requires a pending-transfer state, expiry logic, and either a timeout or an explicit rejection path — all of which expand the attack surface and increase gas/resource cost.

3. **Precedent.** ERC-721 and Superfluid both allow recipients to transfer their stream rights without the original counterparty's approval.

## Consequences

- Senders who care about *who* receives funds (e.g. compliance-constrained grants) should not use Conduit without an application-layer wrapper that enforces an allowlist.
- A future `DripGovernor` upgrade can add an optional `recipient_transfer_locked` flag at stream creation for use cases that need sender control.
- The `xfer_rec` event is emitted on every transfer so indexers can track the full chain of recipients.
