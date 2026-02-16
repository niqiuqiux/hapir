use std::time::{SystemTime, UNIX_EPOCH};

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Clamp an alive-time timestamp: reject non-finite or too-old values,
/// cap future values to now.
pub fn clamp_alive_time(t: i64) -> Option<i64> {
    let now = now_millis();
    if t > now {
        return Some(now);
    }
    // Reject timestamps older than 10 minutes
    if t < now - 10 * 60 * 1000 {
        return None;
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_recent_time() {
        let now = now_millis();
        let recent = now - 5000;
        assert_eq!(clamp_alive_time(recent), Some(recent));
    }

    #[test]
    fn clamp_future_time() {
        let now = now_millis();
        let future = now + 60_000;
        let result = clamp_alive_time(future).unwrap();
        assert!(result <= now + 1); // allow 1ms tolerance
    }

    #[test]
    fn clamp_old_time_returns_none() {
        let now = now_millis();
        let old = now - 15 * 60 * 1000;
        assert_eq!(clamp_alive_time(old), None);
    }
}
