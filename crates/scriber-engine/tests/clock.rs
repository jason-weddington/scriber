//! The local clock. Nothing here asserts a time zone: the gate reruns every
//! test under an unusual one.

#[cfg(test)]
mod tests {
    use scriber_core::library::title;
    use scriber_engine::{Clock as _, LocalClock};

    #[test]
    fn the_clock_reads_a_plausible_local_time() {
        let first = LocalClock.now();
        let second = LocalClock.now();
        for t in [first, second] {
            assert!(t.year >= 2026, "{t:?}");
            assert!((1..=12).contains(&t.month), "{t:?}");
            assert!((1..=31).contains(&t.day), "{t:?}");
            assert!(t.hour <= 23, "{t:?}");
            assert!(t.minute <= 59, "{t:?}");
            assert!(t.second <= 59, "{t:?}");
        }
        assert!(title(&second) >= title(&first), "time moves forward");
    }
}
