pub(crate) const BRIGHTNESS_LOW: u8 = 42;
pub(crate) const BRIGHTNESS_HIGH: u8 = 98;
pub(crate) const CALIBRATED_GAMMA: f64 = 2.4;

#[must_use]
pub(crate) fn hdr_balance(brightness: u8) -> u8 {
    if brightness <= BRIGHTNESS_LOW {
        return 0;
    }
    if brightness >= BRIGHTNESS_HIGH {
        return 100;
    }

    let low = f64::from(BRIGHTNESS_LOW).powf(CALIBRATED_GAMMA);
    let high = f64::from(BRIGHTNESS_HIGH).powf(CALIBRATED_GAMMA);
    let val = f64::from(brightness).powf(CALIBRATED_GAMMA);

    let num = 100.0 * (val - low);
    let den = high - low;
    let target = (num / den).round();
    let mut rounded = 0u8;
    while f64::from(rounded) + 0.5 <= target && rounded < 100 {
        rounded += 1;
    }
    rounded
}

/// Encodes Windows' 0..=100 HDR content-brightness balance as an SDR white level.
///
/// The Settings slider maps linearly from 80 to 480 nits. The `DisplayConfig` fixed-point field is
/// `(nits / 80) * 1000`, hence `1000 + 50 * balance`.
#[must_use]
pub(crate) const fn encode_sdr_white_level(balance: u8) -> u32 {
    1_000 + 50 * balance as u32
}

#[cfg(test)]
mod tests {
    use super::{BRIGHTNESS_HIGH, BRIGHTNESS_LOW, CALIBRATED_GAMMA, encode_sdr_white_level, hdr_balance};

    #[test]
    fn authoritative_pairs_are_exact_and_outside_values_saturate() {
        assert_eq!(BRIGHTNESS_LOW, 42);
        assert_eq!(BRIGHTNESS_HIGH, 98);
        assert!((CALIBRATED_GAMMA - 2.4).abs() < f64::EPSILON);
        assert_eq!(hdr_balance(0), 0);
        assert_eq!(hdr_balance(BRIGHTNESS_LOW), 0);
        assert_eq!(hdr_balance(BRIGHTNESS_HIGH), 100);
        assert_eq!(hdr_balance(100), 100);
    }

    #[test]
    fn interpolation_is_monotone_and_bounded() {
        let values: Vec<_> = (0..=100).map(hdr_balance).collect();
        assert!(values.iter().all(|value| *value <= 100));
        assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(values[43] > values[42]);
        assert!(values[97] < values[98]);
    }

    #[test]
    fn interpolation_matches_supplied_formula_at_interior_points() {
        let expected = [
            (43, 1),
            (50, 8),
            (60, 20),
            (70, 36),
            (80, 56),
            (90, 79),
            (97, 97),
        ];
        for (brightness, balance) in expected {
            assert_eq!(hdr_balance(brightness), balance);
        }
    }

    #[test]
    fn white_level_encoding_matches_windows_slider_endpoints() {
        assert_eq!(encode_sdr_white_level(0), 1_000);
        assert_eq!(encode_sdr_white_level(50), 3_500);
        assert_eq!(encode_sdr_white_level(100), 6_000);
    }
}
