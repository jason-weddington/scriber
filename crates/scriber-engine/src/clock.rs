//! The Mac's local wall-clock time, over jiff.

use jiff::Zoned;
use scriber_core::ports::{Clock, LocalTime};

/// The system clock in the machine's time zone.
pub struct LocalClock;

impl Clock for LocalClock {
    fn now(&self) -> LocalTime {
        let now = Zoned::now();
        LocalTime {
            year: i32::from(now.year()),
            month: u32::from(now.month().unsigned_abs()),
            day: u32::from(now.day().unsigned_abs()),
            hour: u32::from(now.hour().unsigned_abs()),
            minute: u32::from(now.minute().unsigned_abs()),
            second: u32::from(now.second().unsigned_abs()),
        }
    }
}
