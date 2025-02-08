use std::time::SystemTime;

/// Generate a filename that's unique on this computer (among names created
/// by this function).
pub fn unique_filename(prefix: &str, suffix: &str) -> String {
    format!(
        "{prefix}{}-{}{suffix}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
