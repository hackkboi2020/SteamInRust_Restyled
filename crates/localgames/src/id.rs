//! The id Steam gives a non-Steam game, so friends see the same game whether
//! it was added here or as a shortcut in Steam.
//!
//! Steam builds it from a CRC-32 of the quoted program path followed by the
//! display name, with the top bit set; the full 64-bit game id has that value
//! in its high half and `0x02000000` (the "shortcut" type) in the low half.

/// CRC-32 (IEEE 802.3, the one zlib and Steam use).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The 32-bit "app id" of a shortcut.
pub fn shortcut_app_id(exe: &str, name: &str) -> u32 {
    crc32(format!("\"{exe}\"{name}").as_bytes()) | 0x8000_0000
}

/// The 64-bit game id sent to Steam when this game is running.
pub fn game_id(exe: &str, name: &str) -> u64 {
    ((shortcut_app_id(exe, name) as u64) << 32) | 0x0200_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn shortcut_ids_have_the_shortcut_shape() {
        let id = game_id(r"C:\Games\Hades\Hades.exe", "Hades");
        assert_eq!(id & 0xFFFF_FFFF, 0x0200_0000);
        assert!(id >> 63 == 1, "top bit must be set");
        assert_eq!(shortcut_app_id("a", "b") >> 31, 1);
    }

    #[test]
    fn same_input_same_id_different_input_different_id() {
        assert_eq!(game_id("x.exe", "X"), game_id("x.exe", "X"));
        assert_ne!(game_id("x.exe", "X"), game_id("x.exe", "Y"));
    }
}
