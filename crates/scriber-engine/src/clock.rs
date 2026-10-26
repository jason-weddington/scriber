//! The Mac's local wall-clock time, over chrono.

use chrono::{Datelike as _, Local, Timelike as _};
use scriber_core::ports::{Clock, LocalTime};

/// The system clock in the machine's time zone.
pub struct LocalClock;

impl Clock for LocalClock {
    fn now(&self) -> LocalTime {
        let now = Local::now();
        LocalTime {
            year: now.year(),
            month: now.month(),
            day: now.day(),
            hour: now.hour(),
            minute: now.minute(),
            second: now.second(),
        }
    }
}
