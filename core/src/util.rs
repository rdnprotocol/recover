pub fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn le64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}
pub fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
pub fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn be64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

pub fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// FAT/exFAT packed DOS date+time to unix seconds (local time treated as UTC).
pub fn dos_time(date: u16, time: u16) -> Option<i64> {
    if date == 0 {
        return None;
    }
    let y = 1980 + (date >> 9) as i64;
    let m = ((date >> 5) & 0xF) as i64;
    let d = (date & 0x1F) as i64;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let hh = (time >> 11) as i64;
    let mm = ((time >> 5) & 0x3F) as i64;
    let ss = ((time & 0x1F) * 2) as i64;
    Some(days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Windows FILETIME (100ns since 1601) to unix seconds.
pub fn filetime(ft: u64) -> Option<i64> {
    if ft == 0 {
        return None;
    }
    let s = (ft / 10_000_000) as i64 - 11_644_473_600;
    if !(0..=4_102_444_800).contains(&s) {
        return None;
    }
    Some(s)
}

/// Make a name safe for Windows/Unix filesystems.
pub fn sanitize_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        s = "_".into();
    }
    let upper = s.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "LPT1" | "LPT2" | "LPT3") {
        s.insert(0, '_');
    }
    if s.chars().count() > 200 {
        s = s.chars().take(200).collect();
    }
    s
}
