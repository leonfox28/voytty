use ctutils::CtEq;
use noq_proto::crypto;
use rand::RngExt;
use rustls::crypto::cipher::{
    AeadKey, InboundOpaqueMessage, Iv, NONCE_LEN, OutboundPlainMessage, Tls13AeadAlgorithm,
};

/// Implements [`crypto::HandshakeTokenKey`] using a [`Tls13AeadAlgorithm`].
///
/// This can be obtained from looking through available ciphers from a
/// [`rustls::crypto::CryptoProvider`].
pub(crate) struct RustlsTokenKey {
    key: [u8; 32],
    aead: &'static dyn Tls13AeadAlgorithm,
}

impl RustlsTokenKey {
    /// Constructs [`crypto::HandshakeTokenKey`] from a [`rustls::crypto::CryptoProvider`].
    ///
    /// Tries to find a suitable TLS 1.3 cipher suite from the provided crypto provider,
    /// then uses it to extract an AEAD to use as the token key encryption method.
    ///
    /// Then generates a random master key to use.
    ///
    /// Returns `None` when this can't find a suitable TLS cipher suite in the given crypto
    /// provider.
    pub(crate) fn new(
        rng: &mut impl rand::CryptoRng,
        crypto_provider: &rustls::crypto::CryptoProvider,
    ) -> Option<Self> {
        let suite = crypto_provider
            .cipher_suites
            .iter()
            .filter_map(|suite| suite.tls13())
            .next()?;
        let aead = suite.aead_alg;
        Some(Self {
            key: rng.random(),
            aead,
        })
    }
}

impl crypto::HandshakeTokenKey for RustlsTokenKey {
    fn seal(&self, token_nonce: u128, data: &mut Vec<u8>) -> Result<(), crypto::CryptoError> {
        let key = AeadKey::from(self.key);
        let nonce: [u8; NONCE_LEN] = *token_nonce
            .to_le_bytes()
            .first_chunk()
            .expect("expected u128 > 96 bit");
        let iv = Iv::from(nonce);
        let msg = OutboundPlainMessage {
            typ: rustls::ContentType::ApplicationData,
            version: rustls::ProtocolVersion::TLSv1_3,
            payload: rustls::crypto::cipher::OutboundChunks::Single(&*data),
        };
        let out = self
            .aead
            .encrypter(key, iv)
            .encrypt(msg, 0)
            .map_err(|_| crypto::CryptoError)?;

        data.clear();
        data.extend(out.payload.as_ref());

        Ok(())
    }

    fn open<'a>(
        &self,
        token_nonce: u128,
        data: &'a mut [u8],
    ) -> Result<&'a [u8], crypto::CryptoError> {
        let key = AeadKey::from(self.key);
        let nonce: [u8; NONCE_LEN] = *token_nonce
            .to_le_bytes()
            .first_chunk()
            .expect("expected u128 > 96 bit");
        let iv = Iv::from(nonce);

        let msg = InboundOpaqueMessage::new(
            rustls::ContentType::ApplicationData,
            rustls::ProtocolVersion::TLSv1_3,
            data,
        );
        let plain = self
            .aead
            .decrypter(key, iv)
            .decrypt(msg, 0)
            .map_err(|_| crypto::CryptoError)?;

        Ok(plain.payload)
    }
}

pub(crate) struct Blake3HmacKey([u8; 32]);

impl Blake3HmacKey {
    pub(crate) fn for_stateless_reset(secret: &iroh_base::SecretKey) -> Self {
        Self(blake3::derive_key(
            "iroh zterm persistent stateless reset v1",
            &secret.to_bytes(),
        ))
    }

    pub(crate) fn new(rng: &mut impl rand::CryptoRng) -> Self {
        let mut key = [0u8; 32];
        rng.fill_bytes(&mut key);
        Self(key)
    }
}

impl crypto::HmacKey for Blake3HmacKey {
    fn sign(&self, data: &[u8], signature_out: &mut [u8]) {
        signature_out.copy_from_slice(blake3::keyed_hash(&self.0, data).as_slice());
    }

    fn signature_len(&self) -> usize {
        blake3::OUT_LEN // 32 bytes
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), crypto::CryptoError> {
        let reference = blake3::keyed_hash(&self.0, data);
        // to_bool is fine here, because it's the last thing we do to
        // distinguish success or failure (see to_bool documentation)
        if signature.ct_eq(reference.as_slice()).to_bool() {
            Ok(())
        } else {
            Err(crypto::CryptoError)
        }
    }
}

#[cfg(test)]
mod reset_tests {
    use super::*;
    use crypto::HmacKey;

    #[test]
    fn zterm_reset_key_is_stable_identity_scoped_and_domain_separated() {
        let identity = iroh_base::SecretKey::generate();
        let first = Blake3HmacKey::for_stateless_reset(&identity);
        let restarted = Blake3HmacKey::for_stateless_reset(&identity);
        let rotated = Blake3HmacKey::for_stateless_reset(&iroh_base::SecretKey::generate());
        let raw_identity = Blake3HmacKey(identity.to_bytes());
        let other_purpose = Blake3HmacKey(blake3::derive_key(
            "other protocol purpose",
            &identity.to_bytes(),
        ));
        let cid = [41; 16];
        let mut proof = [0; blake3::OUT_LEN];
        first.sign(&cid, &mut proof);
        assert!(restarted.verify(&cid, &proof).is_ok());
        assert!(rotated.verify(&cid, &proof).is_err());
        assert!(raw_identity.verify(&cid, &proof).is_err());
        assert!(other_purpose.verify(&cid, &proof).is_err());
        assert!(first.verify(&[42; 16], &proof).is_err());
    }

    #[test]
    fn zterm_ephemeral_reset_keys_remain_independent() {
        let first = Blake3HmacKey::new(&mut rand::rng());
        let next = Blake3HmacKey::new(&mut rand::rng());
        let mut proof = [0; blake3::OUT_LEN];
        first.sign(&[41; 16], &mut proof);
        assert!(next.verify(&[41; 16], &proof).is_err());
    }
}
