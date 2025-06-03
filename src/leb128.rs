/// Convert a u32 to LEB128 encoding. This is used in the WASM binary format.
/// I got a little bit carried away here making a version without many branches.
/// I have exhaustively tested that it is correct using the `exhaustive_leb128_test()`
/// test below.
pub fn u32_to_leb128(n: u32) -> Vec<u8> {
    let mut leb = vec![
        ((n >> 0) as u8 & 0x7F) | (if n >> 7 == 0 { 0 } else { 0x80 }),
        ((n >> 7) as u8 & 0x7F) | (if n >> 14 == 0 { 0 } else { 0x80 }),
        ((n >> 14) as u8 & 0x7F) | (if n >> 21 == 0 { 0 } else { 0x80 }),
        ((n >> 21) as u8 & 0x7F) | (if n >> 28 == 0 { 0 } else { 0x80 }),
        ((n >> 28) as u8 & 0x7F),
    ];
    let num_bytes = n.checked_ilog2().map(|n: u32| n / 7 + 1).unwrap_or(1);
    leb.resize(num_bytes as usize, 0);
    leb
}

/// Convert leb128 to u32, returning the value and number of bytes read.
/// If there are insufficient bytes we return None.
pub fn leb128_to_u32(leb: &[u8]) -> Option<(u32, usize)> {
    let mut n = 0u32;
    for i in 0..5 {
        let byte = leb.get(i)?;
        n |= ((byte & 0x7F) as u32) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((n, i + 1));
        }
    }
    Some((n, 5))
}

#[cfg(test)]
mod test {
    use super::*;

    fn u32_to_leb128_simple(n: u32) -> Vec<u8> {
        let mut leb = Vec::with_capacity(5);

        for septet in 0..5 {
            let byte = (n >> (7 * septet)) as u8 & 0x7F;
            if septet == 4 || (n >> (7 * (septet + 1))) == 0 {
                // Last byte.
                leb.push(byte);
                break;
            } else {
                leb.push(byte | 0x80);
            }
        }
        leb
    }

    /// To run:
    ///
    ///   cargo test --release -- --ignored exhaustive_leb128_test
    ///
    /// It takes about 5 minutes.
    #[test]
    #[ignore]
    fn exhaustive_leb128_test() {
        for i in 0..=u32::MAX {
            let correct_encoding = u32_to_leb128_simple(i);
            assert_eq!(u32_to_leb128(i), correct_encoding);
            assert!(matches!(leb128_to_u32(&correct_encoding), Some((v, _)) if v == i));
        }
    }
}
