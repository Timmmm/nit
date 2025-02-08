use std::hash::{Hash, Hasher};

/// Hash an `impl Hash` with a `Digest`. By default you can only use
/// `Hasher` which only gives a u64 output. This allows you to output
/// longer hashes.
pub fn hash_digest<H: Hash>(hash: H, digest: blake3::Hasher) -> blake3::Hash {
    let mut digest_hasher = DigestHasher { digest };
    hash.hash(&mut digest_hasher);
    digest_hasher.digest.finalize()
}

pub struct DigestHasher {
    digest: blake3::Hasher,
}

impl Hasher for DigestHasher {
    fn finish(&self) -> u64 {
        unimplemented!("Do not call finish()");
    }

    fn write(&mut self, bytes: &[u8]) {
        self.digest.update(bytes);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Hash)]
    struct TestHash {
        name: String,
        age: u64,
    }

    #[test]
    fn test_hash_digest() {
        let hash = TestHash {
            name: "dave".to_string(),
            age: 5,
        };
        let digest = blake3::Hasher::new();
        let output = hash_digest(hash, digest);
        assert_eq!(
            output.to_hex().to_string(),
            "8b8ae45bfe6457ca91313ef2dd51b2e94aa93615ab85e2f833f9c8cfaa81c183"
        );
    }
}
