//! The local clock. The gate also reruns every test under `TZ=Pacific/Chatham`.

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::{Duration, SystemTime};

    use jiff::Timestamp;
    use jiff::tz::TimeZone;
    use scriber_core::ports::LocalTime;
    use scriber_engine::{Clock as _, LocalClock};

    const MAX_BRACKET_ATTEMPTS: usize = 5;
    const MAX_BRACKET_WIDTH: Duration = Duration::from_secs(2);

    fn assert_plausible(t: LocalTime) {
        assert!(t.year >= 2026, "{t:?}");
        assert!((1..=12).contains(&t.month), "{t:?}");
        assert!((1..=31).contains(&t.day), "{t:?}");
        assert!(t.hour <= 23, "{t:?}");
        assert!(t.minute <= 59, "{t:?}");
        assert!(t.second <= 59, "{t:?}");
    }

    fn sample_bracketed() -> (Timestamp, LocalTime, Timestamp) {
        let mut last_failure = String::new();
        for _ in 0..MAX_BRACKET_ATTEMPTS {
            let before = SystemTime::now();
            let sample = LocalClock.now();
            let after = SystemTime::now();
            let width = match after.duration_since(before) {
                Ok(width) if width <= MAX_BRACKET_WIDTH => width,
                Ok(width) => {
                    last_failure =
                        format!("bracket width {width:?} exceeded {MAX_BRACKET_WIDTH:?}");
                    continue;
                }
                Err(err) => {
                    last_failure = format!("clock stepped backward across bracket: {err}");
                    continue;
                }
            };
            match (Timestamp::try_from(before), Timestamp::try_from(after)) {
                (Ok(start), Ok(end)) if start <= end => return (start, sample, end),
                (start, end) => {
                    last_failure = format!(
                        "invalid SystemTime bracket (width {width:?}): start={start:?}, end={end:?}"
                    );
                }
            }
        }
        panic!(
            "failed to capture a valid SystemTime bracket within {MAX_BRACKET_ATTEMPTS} attempts: {last_failure}"
        );
    }

    #[test]
    fn the_clock_reads_a_plausible_local_time() {
        let (before, actual, after) = sample_bracketed();
        assert_plausible(actual);

        if std::env::var_os("TZ").as_deref() == Some(OsStr::new("Pacific/Chatham")) {
            let chatham = TimeZone::get("Pacific/Chatham")
                .unwrap_or_else(|e| panic!("failed to resolve named zone Pacific/Chatham: {e}"));
            let start_sec = before.as_second();
            let end_sec = after.as_second();
            let mut expected = Vec::new();
            for sec in start_sec..=end_sec {
                let ts = Timestamp::from_second(sec)
                    .unwrap_or_else(|e| panic!("invalid whole-second timestamp {sec}: {e}"));
                let zoned = ts.to_zoned(chatham.clone());
                expected.push(LocalTime {
                    year: i32::from(zoned.year()),
                    month: u32::from(zoned.month().unsigned_abs()),
                    day: u32::from(zoned.day().unsigned_abs()),
                    hour: u32::from(zoned.hour().unsigned_abs()),
                    minute: u32::from(zoned.minute().unsigned_abs()),
                    second: u32::from(zoned.second().unsigned_abs()),
                });
            }
            assert!(
                expected.contains(&actual),
                "LocalClock::now() returned {actual:?}, outside Pacific/Chatham bracket {expected:?} (seconds {start_sec}..={end_sec})"
            );
        }
    }
}
