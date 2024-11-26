use std::time::{SystemTime, UNIX_EPOCH};

pub fn file_name_from_str(s: &str) -> String {
    String::from(
        s.rsplit_once(std::path::MAIN_SEPARATOR)
            .map_or(s, |(_, b)| b),
    )
}

pub fn bool_from_str(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().trim(),
        "true" | "t" | "1" | "yes" | "y" | "on"
    )
}

pub fn current_timestamp() -> i64 {
    let start = SystemTime::now();
    let timestamp = start
        .duration_since(UNIX_EPOCH)
        .expect("Current time is before UNIX epoch")
        .as_secs();
    i64::try_from(timestamp).expect("Timestamp overflow")
}
