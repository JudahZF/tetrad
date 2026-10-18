//! XXTEA (`btea`) as stock `0x2a532c` applies it to the key material. Words are
//! little endian: `0x2a532c` copies the bytes into its word array (`memcpy`) on
//! the little-endian ARM.

use zeroize::Zeroizing;

const DELTA: u32 = 0x9e37_79b9;

/// Encrypts `data` as LE words, zero padded to a 4-byte multiple, with the
/// byte length appended as one word. The plaintext word copy is zeroed on drop
/// (owned; stock `0x2a532c` does not zero its copies).
pub(crate) fn encrypt(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut words = to_words(data);
    words.push(data.len() as u32);
    btea_encrypt(&mut words, &key_words(key));
    to_bytes(&words)
}

fn btea_encrypt(v: &mut [u32], key: &[u32; 4]) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut rounds = 6 + 52 / n;
    let mut sum = 0u32;
    let mut z = v[n - 1];
    while rounds > 0 {
        sum = sum.wrapping_add(DELTA);
        let e = ((sum >> 2) & 3) as usize;
        for p in 0..n - 1 {
            let y = v[p + 1];
            v[p] = v[p].wrapping_add(mix(sum, y, z, p, e, key));
            z = v[p];
        }
        let y = v[0];
        v[n - 1] = v[n - 1].wrapping_add(mix(sum, y, z, n - 1, e, key));
        z = v[n - 1];
        rounds -= 1;
    }
}

fn mix(sum: u32, y: u32, z: u32, p: usize, e: usize, key: &[u32; 4]) -> u32 {
    ((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4))
        ^ (sum ^ y).wrapping_add(key[(p & 3) ^ e] ^ z)
}

/// Zeroed on drop, as the key it comes from.
fn key_words(key: &[u8; 16]) -> Zeroizing<[u32; 4]> {
    Zeroizing::new(std::array::from_fn(|i| {
        u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]])
    }))
}

/// Room for the length word: the push in [`encrypt`] must not reallocate and
/// free an unzeroed plaintext copy.
fn to_words(data: &[u8]) -> Zeroizing<Vec<u32>> {
    let mut words = Zeroizing::new(Vec::with_capacity(data.len().div_ceil(4) + 1));
    words.extend(data.chunks(4).map(|chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        u32::from_le_bytes(word)
    }));
    words
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decrypt(key: [u8; 16], data: &[u8]) -> Vec<u8> {
        let mut v = to_words(data);
        let n = v.len();
        let key = key_words(&key);
        if n >= 2 {
            let mut rounds = 6 + 52 / n;
            let mut sum = DELTA.wrapping_mul(rounds as u32);
            let mut y = v[0];
            while rounds > 0 {
                let e = ((sum >> 2) & 3) as usize;
                for p in (1..n).rev() {
                    let z = v[p - 1];
                    v[p] = v[p].wrapping_sub(mix(sum, y, z, p, e, &key));
                    y = v[p];
                }
                let z = v[n - 1];
                v[0] = v[0].wrapping_sub(mix(sum, y, z, 0, e, &key));
                y = v[0];
                sum = sum.wrapping_sub(DELTA);
                rounds -= 1;
            }
        }
        to_bytes(&v)
    }

    /// Known answers: the public all-zero vector, and a 3-word vector from the
    /// published `btea` reference C (Wheeler and Needham, 1998) compiled on the
    /// host. A wrong `mix` passes the round trip below, not these.
    #[test]
    fn btea_matches_known_answers() {
        let mut zero = [0u32; 2];
        btea_encrypt(&mut zero, &[0; 4]);
        assert_eq!(zero, [0x0537_04ab, 0x575d_8c80]);

        // LE key words 0x00010203, 0x04050607, 0x08090a0b, 0x0c0d0e0f.
        let key = [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12];
        let mut words = [0x0011_2233, 0x4455_6677, 0x8899_aabb];
        btea_encrypt(&mut words, &key_words(&key));
        assert_eq!(words, [0x77bd_c37a, 0xb944_8c9f, 0x01e4_7a32]);
    }

    /// Review r15 R17: the length-word push must not reallocate, or the old
    /// plaintext buffer is freed without zeroing.
    #[test]
    fn plaintext_words_have_room_for_the_length_word() {
        for len in [0, 1, 4, 17] {
            let words = to_words(&vec![0xa5; len]);
            assert!(words.capacity() > words.len(), "{len} bytes");
        }
    }

    #[test]
    fn decrypt_recovers_padded_data_and_length() {
        let key = *b"0123456789abcdef";
        let data = b"vecB key material";
        let mut plain = data.to_vec();
        plain.resize(20, 0);
        plain.extend_from_slice(&17u32.to_le_bytes());

        let cipher = encrypt(&key, data);
        assert_ne!(cipher, plain);
        assert_eq!(decrypt(key, &cipher), plain);
    }
}
