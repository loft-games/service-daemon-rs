use uuid::Uuid;

/// Generates a globally unique, time-ordered message ID for each trigger event.
///
/// Produces a UUID v7 whose high bits encode a millisecond-precision timestamp,
/// guaranteeing lexicographic ordering that mirrors chronological ordering.
/// Returns the `Uuid` value directly (16 bytes, Copy, zero heap allocation).
///
/// This is also called by the public `context::generate_message_id()` API.
pub(crate) fn generate_message_id() -> Uuid {
    Uuid::now_v7()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_monotonic_ordering() {
        let mut previous = generate_message_id();
        for _ in 0..100 {
            let current = generate_message_id();
            assert!(
                current >= previous,
                "UUID v7 IDs must be monotonically non-decreasing: previous={}, current={}",
                previous,
                current
            );
            previous = current;
        }
    }
}
