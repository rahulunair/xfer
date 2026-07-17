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
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = expected_byte(index, seed);
    }
}

fn verify_pattern(buffer: &[u8], seed: u8) -> std::result::Result<(), VerificationMismatch> {
    for (index, actual) in buffer.iter().copied().enumerate() {
        let expected = expected_byte(index, seed);
        if actual != expected {
            return Err(VerificationMismatch {
                offset: index,
                expected,
                actual,
            });
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

fn expected_byte(index: usize, seed: u8) -> u8 {
    let low = index.to_le_bytes()[0];
    let mixed = low.wrapping_mul(31).rotate_left(3);
    seed ^ mixed.wrapping_add(0x3d)
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
}
