use soroban_sdk::{contracttype, Bytes, BytesN, Env};

use crate::Error;

/// Domain separator prefix for cross-chain stream signatures.
/// Combined with the factory contract address to produce a unique domain hash
/// per deployment, preventing signature replay across different factory instances.
const DOMAIN_SEPARATOR_PREFIX: &[u8; 26] = b"Conduit Stream Creation v1";

/// The payload that gets signed off-chain for cross-chain stream creation.
///
/// All fields that affect stream behavior are included so the on-chain verifier
/// can reconstruct the exact same hash and confirm the signer authorized these
/// specific parameters on this specific network.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignaturePayload {
    pub sender: BytesN<32>,
    pub recipient: BytesN<32>,
    pub token: BytesN<32>,
    pub deposit: i128,
    pub rate_per_sec: i128,
    pub start_time: u64,
    pub end_time: u64,
    pub clawback: bool,
    pub nonce: u64,
    pub deadline: u64,
    pub network_passphrase: Bytes,
}

/// Encode a `bool` as a single byte (0x00 or 0x01).
fn encode_bool(val: bool) -> u8 {
    if val {
        1
    } else {
        0
    }
}

/// Encode an `i128` as 16 big-endian bytes.
fn encode_i128(val: i128) -> [u8; 16] {
    val.to_be_bytes()
}

/// Encode a `u64` as 8 big-endian bytes.
fn encode_u64(val: u64) -> [u8; 8] {
    val.to_be_bytes()
}

/// Compute the domain-separated hash of a `SignaturePayload`.
///
/// The hash binds the signature to:
/// - This specific factory contract address (prevents cross-instance replay)
/// - The "Conduit Stream Creation v1" domain tag (prevents cross-protocol replay)
/// - All stream parameters (prevents parameter tampering)
/// - The Stellar network passphrase (prevents cross-network replay)
/// - A per-sender nonce (prevents same-network replay)
///
/// Returns a 32-byte SHA-256 digest suitable for ed25519 signature verification.
pub fn hash_payload(env: &Env, factory_address: &BytesN<32>, payload: &SignaturePayload) -> BytesN<32> {
    // ── Domain hash: SHA256(prefix || factory_address) ──────────────────
    let mut domain_input = Bytes::new(env);
    domain_input.extend_from_slice(DOMAIN_SEPARATOR_PREFIX);
    domain_input.extend_from_slice(&factory_address.to_array());
    let domain_hash = env.crypto().sha256(&domain_input);

    // ── Struct hash: SHA256(all fields concatenated) ────────────────────
    let mut struct_input = Bytes::new(env);
    // sender (32 bytes)
    struct_input.extend_from_slice(&payload.sender.to_array());
    // recipient (32 bytes)
    struct_input.extend_from_slice(&payload.recipient.to_array());
    // token (32 bytes)
    struct_input.extend_from_slice(&payload.token.to_array());
    // deposit (16 bytes)
    struct_input.extend_from_slice(&encode_i128(payload.deposit));
    // rate_per_sec (16 bytes)
    struct_input.extend_from_slice(&encode_i128(payload.rate_per_sec));
    // start_time (8 bytes)
    struct_input.extend_from_slice(&encode_u64(payload.start_time));
    // end_time (8 bytes)
    struct_input.extend_from_slice(&encode_u64(payload.end_time));
    // clawback (1 byte)
    struct_input.push_back(encode_bool(payload.clawback));
    // nonce (8 bytes)
    struct_input.extend_from_slice(&encode_u64(payload.nonce));
    // deadline (8 bytes)
    struct_input.extend_from_slice(&encode_u64(payload.deadline));
    // network_passphrase (variable length)
    struct_input.extend_from_slice(&payload.network_passphrase);
    let struct_hash = env.crypto().sha256(&struct_input);

    // ── Final digest: SHA256(domain_hash || struct_hash) ────────────────
    let mut final_input = Bytes::new(env);
    final_input.extend_from_slice(&domain_hash.to_array());
    final_input.extend_from_slice(&struct_hash.to_array());
    env.crypto().sha256(&final_input)
}

/// Verify an ed25519 signature over a `SignaturePayload`.
///
/// Reconstructs the domain-separated hash from the payload and checks it
/// against the provided signature using the given public key.
pub fn verify_signature(
    env: &Env,
    factory_address: &BytesN<32>,
    payload: &SignaturePayload,
    public_key: &BytesN<32>,
    signature: &BytesN<64>,
) -> Result<(), Error> {
    let digest = hash_payload(env, factory_address, payload);
    env.crypto()
        .ed25519_verify(public_key, &digest.to_buffer(), signature);
    Ok(())
}
