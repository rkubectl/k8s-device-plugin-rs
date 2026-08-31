/// Linux's `sockaddr_un.sun_path` is 108 bytes including the NUL terminator;
/// stay one byte under that as a conservative, portable budget.
pub const MAX_SOCKET_PATH_LEN: usize = 107;

/// Derives a filesystem-safe, collision-resistant socket name from `name`,
/// never longer than `max_len`.
///
/// Sanitization alone is not injective (e.g. "acme.com/gpu" and "acme_com/gpu" both
/// sanitize to "acme_com_gpu"), so a deterministic hash of the *original* name is
/// appended to make collisions impractical. The human-readable sanitized part is
/// truncated (never the hash) if needed so the result never exceeds `max_len`.
/// Budgets below 17 bytes cannot retain the complete hash; those return a
/// deterministic hash prefix while still honoring the length limit.
///
/// `max_len` is the caller's total budget for the returned string -- callers
/// building a full socket path subtract their own path prefix (and any
/// literal suffix they'll append) from [`MAX_SOCKET_PATH_LEN`] first.
pub fn sanitize_socket_name(name: &str, max_len: usize) -> String {
    let sanitized = name.replace(invalid_char, "_");
    let suffix = format!("-{:016x}", fnv1a64(name.as_bytes()));
    if max_len < suffix.len() {
        return suffix[..max_len].to_string();
    }
    let budget = max_len.saturating_sub(suffix.len());
    // `sanitized` is guaranteed pure ASCII (invalid_char maps everything else to
    // '_'), so counting chars is equivalent to counting bytes here.
    let truncated = sanitized.chars().take(budget).collect::<String>();
    truncated + &suffix
}

fn invalid_char(c: char) -> bool {
    !(c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_socket_name_is_deterministic() {
        assert_eq!(
            sanitize_socket_name("example.com/device", 100),
            sanitize_socket_name("example.com/device", 100)
        );
    }

    #[test]
    fn sanitize_socket_name_does_not_collide_across_distinct_names() {
        // These two names sanitize to the same "acme_com_gpu" prefix but must not
        // collide once the disambiguating hash suffix is applied.
        assert_ne!(
            sanitize_socket_name("acme.com/gpu", 100),
            sanitize_socket_name("acme_com/gpu", 100)
        );
    }

    #[test]
    fn sanitize_socket_name_keeps_result_within_max_len() {
        let long_name = "example.com/a-very-long-custom-accelerator-resource-name-that-keeps-going";
        let max_len = 50;
        let result = sanitize_socket_name(long_name, max_len);

        assert!(
            result.len() <= max_len,
            "result length {} exceeds max_len {max_len}",
            result.len()
        );
        // The disambiguating hash suffix (`-` + 16 hex digits) must survive
        // truncation intact.
        let suffix = &result[result.len() - 17..];
        assert!(suffix.starts_with('-'));
        assert!(suffix[1..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sanitize_socket_name_honors_tiny_budgets() {
        for max_len in 0..17 {
            let result = sanitize_socket_name("example.com/device", max_len);
            assert_eq!(result.len(), max_len);
        }
    }
}
