//! Optional age encryption for batch bodies. The manifest stays readable.

use age::secrecy::ExposeSecret;

pub fn encrypt_batch(plaintext: &[u8], recipient: &str) -> anyhow::Result<Vec<u8>> {
    let recipient: age::x25519::Recipient =
        recipient.parse().map_err(|err: &str| anyhow::anyhow!("the sync key recipient is not an age key: {err}"))?;
    age::encrypt(&recipient, plaintext).map_err(|err| anyhow::anyhow!("the batch could not be encrypted: {err}"))
}

pub fn decrypt_batch(ciphertext: &[u8], identity: &str) -> anyhow::Result<Vec<u8>> {
    let identity: age::x25519::Identity =
        identity.parse().map_err(|err: &str| anyhow::anyhow!("the sync identity is not an age key: {err}"))?;
    if age::Decryptor::new_buffered(ciphertext).is_ok_and(|decryptor| decryptor.is_scrypt()) {
        anyhow::bail!("the batch uses a passphrase, not a sync key");
    }
    age::decrypt(&identity, ciphertext).map_err(|err| anyhow::anyhow!("the batch could not be decrypted: {err}"))
}

pub fn is_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(b"age-encryption.org/") || bytes.starts_with(b"-----BEGIN AGE")
}

/// A new age identity and its public recipient, both as stored text.
/// The settings screen does not create a key yet; exchange uses this when a recipient is set.
#[allow(dead_code)]
pub fn generate_identity() -> (String, String) {
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();
    (identity.to_string().expose_secret().to_string(), recipient)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_round_trips_through_age() {
        let (identity, recipient) = generate_identity();
        let encrypted = encrypt_batch(b"{\"ops\":[]}", &recipient).unwrap();
        let needle = b"\"ops\"";
        assert!(is_encrypted(&encrypted));
        assert!(!encrypted.windows(needle.len()).any(|w| w == needle));
        let plain = decrypt_batch(&encrypted, &identity).unwrap();
        assert_eq!(plain, b"{\"ops\":[]}");
    }
}
