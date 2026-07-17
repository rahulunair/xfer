use super::error::{BenchmarkError, CaseExecutionError};

pub(crate) const PATTERN_SEED: u8 = 0x5a;
pub(crate) const SENTINEL_SEED: u8 = 0xa5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerificationMismatch {
    offset: usize,
    expected: u8,
    actual: u8,
}

pub(crate) fn fill_pattern(buffer: &mut [u8], seed: u8) {
    for (word_index, chunk) in buffer.chunks_mut(8).enumerate() {
        let expected = pattern_word(word_index, seed).to_le_bytes();
        chunk.copy_from_slice(&expected[..chunk.len()]);
    }
}

fn verify_pattern(buffer: &[u8], seed: u8) -> std::result::Result<(), VerificationMismatch> {
    for (word_index, chunk) in buffer.chunks(8).enumerate() {
        let expected = pattern_word(word_index, seed).to_le_bytes();
        for (byte_index, actual) in chunk.iter().copied().enumerate() {
            if actual != expected[byte_index] {
                return Err(VerificationMismatch {
                    offset: word_index * 8 + byte_index,
                    expected: expected[byte_index],
                    actual,
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn verify_or_fail(
    buffer: &[u8],
    seed: u8,
    case: &str,
) -> std::result::Result<(), CaseExecutionError> {
    verify_pattern(buffer, seed).map_err(|mismatch| {
        CaseExecutionError::Fatal(BenchmarkError::VerificationFailed {
            case: case.to_owned(),
            offset: mismatch.offset,
            expected: mismatch.expected,
            actual: mismatch.actual,
        })
    })
}

#[cfg(test)]
fn expected_byte(index: usize, seed: u8) -> u8 {
    pattern_word(index / 8, seed).to_le_bytes()[index % 8]
}

fn pattern_word(word_index: usize, seed: u8) -> u64 {
    let mut value = (word_index as u64) ^ (u64::from(seed) << 56) ^ 0x5846_4552_5041_5454;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_verification_finds_first_bad_byte() {
        let mut bytes = vec![0; 64];
        fill_pattern(&mut bytes, PATTERN_SEED);
        bytes[17] ^= 1;

        assert_eq!(
            verify_pattern(&bytes, PATTERN_SEED),
            Err(VerificationMismatch {
                offset: 17,
                expected: expected_byte(17, PATTERN_SEED),
                actual: expected_byte(17, PATTERN_SEED) ^ 1,
            })
        );
    }

    #[test]
    fn pattern_does_not_repeat_at_256_byte_boundaries() {
        let mut bytes = vec![0; 768];
        fill_pattern(&mut bytes, PATTERN_SEED);

        assert_ne!(&bytes[..256], &bytes[256..512]);
        assert_ne!(&bytes[256..512], &bytes[512..]);
    }
}
