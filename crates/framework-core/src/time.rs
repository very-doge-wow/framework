use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// A rational number, used for frame rates and time bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            f64::from(self.num) / f64::from(self.den)
        }
    }

    /// Duration of one unit (e.g. one frame for a frame rate).
    pub fn frame_duration(self) -> Time {
        Time::from_frames(1, self)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{:.3}", self.as_f64())
        }
    }
}

/// A point in time or a duration, measured in flicks.
///
/// One flick is 1/705_600_000 s. That unit divides evenly into every common
/// video frame rate (24, 25, 30, 48, 50, 60, 90, 100, 120 and their 1000/1001
/// variants) and audio sample rate (8k – 192k), so frame and sample positions
/// are exact integers.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const FLICKS_PER_SECOND: i64 = 705_600_000;
    pub const ZERO: Time = Time(0);

    pub const fn from_flicks(flicks: i64) -> Self {
        Self(flicks)
    }

    pub const fn flicks(self) -> i64 {
        self.0
    }

    pub fn from_seconds(seconds: f64) -> Self {
        Self((seconds * Self::FLICKS_PER_SECOND as f64).round() as i64)
    }

    pub fn as_seconds(self) -> f64 {
        self.0 as f64 / Self::FLICKS_PER_SECOND as f64
    }

    /// Converts `frames` at `rate` frames per second into a time.
    pub fn from_frames(frames: i64, rate: Rational) -> Self {
        if rate.num == 0 {
            return Self::ZERO;
        }
        let flicks =
            i128::from(frames) * i128::from(Self::FLICKS_PER_SECOND) * i128::from(rate.den)
                / i128::from(rate.num);
        Self(flicks as i64)
    }

    /// Index of the frame (at `rate`) that contains this time.
    pub fn to_frames_floor(self, rate: Rational) -> i64 {
        if rate.den == 0 {
            return 0;
        }
        let num = i128::from(self.0) * i128::from(rate.num);
        let den = i128::from(Self::FLICKS_PER_SECOND) * i128::from(rate.den);
        num.div_euclid(den) as i64
    }

    /// Converts a timestamp expressed in a `num/den` time base (as used by FFmpeg).
    pub fn from_timebase(ts: i64, num: i32, den: i32) -> Self {
        if den == 0 {
            return Self::ZERO;
        }
        let flicks = i128::from(ts) * i128::from(Self::FLICKS_PER_SECOND) * i128::from(num)
            / i128::from(den);
        Self(flicks as i64)
    }

    /// Converts this time into a timestamp in a `num/den` time base (rounding down).
    pub fn to_timebase(self, num: i32, den: i32) -> i64 {
        if num == 0 {
            return 0;
        }
        let n = i128::from(self.0) * i128::from(den);
        let d = i128::from(Self::FLICKS_PER_SECOND) * i128::from(num);
        n.div_euclid(d) as i64
    }

    /// Number of whole samples at `sample_rate` covered by this time.
    pub fn to_samples(self, sample_rate: u32) -> i64 {
        (i128::from(self.0) * i128::from(sample_rate))
            .div_euclid(i128::from(Self::FLICKS_PER_SECOND)) as i64
    }

    pub fn from_samples(samples: i64, sample_rate: u32) -> Self {
        if sample_rate == 0 {
            return Self::ZERO;
        }
        Self(
            (i128::from(samples) * i128::from(Self::FLICKS_PER_SECOND) / i128::from(sample_rate))
                as i64,
        )
    }

    pub fn max(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }

    pub fn min(self, other: Self) -> Self {
        if self <= other { self } else { other }
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, rhs: Time) -> Time {
        Time(self.0 + rhs.0)
    }
}

impl AddAssign for Time {
    fn add_assign(&mut self, rhs: Time) {
        self.0 += rhs.0;
    }
}

impl Sub for Time {
    type Output = Time;
    fn sub(self, rhs: Time) -> Time {
        Time(self.0 - rhs.0)
    }
}

impl SubAssign for Time {
    fn sub_assign(&mut self, rhs: Time) {
        self.0 -= rhs.0;
    }
}

impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}

impl fmt::Display for Time {
    /// Formats as `HH:MM:SS.mmm`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let total_ms =
            (self.0.unsigned_abs() as u128 * 1000 / Self::FLICKS_PER_SECOND as u128) as u64;
        let ms = total_ms % 1000;
        let s = (total_ms / 1000) % 60;
        let m = (total_ms / 60_000) % 60;
        let h = total_ms / 3_600_000;
        write!(f, "{sign}{h:02}:{m:02}:{s:02}.{ms:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_frame_rates_are_exact() {
        for (num, den) in [
            (24, 1),
            (25, 1),
            (30, 1),
            (50, 1),
            (60, 1),
            (120, 1),
            (24000, 1001),
            (30000, 1001),
            (60000, 1001),
        ] {
            let rate = Rational::new(num, den);
            let one = Time::from_frames(1, rate);
            assert_eq!(
                i128::from(one.0) * i128::from(num),
                i128::from(Time::FLICKS_PER_SECOND) * i128::from(den),
                "{num}/{den} is not exact"
            );
            assert_eq!(Time::from_frames(1234, rate).to_frames_floor(rate), 1234);
        }
    }

    #[test]
    fn sample_rates_are_exact() {
        for rate in [8000, 22050, 44100, 48000, 96000, 192000] {
            assert_eq!(Time::FLICKS_PER_SECOND % rate, 0);
            assert_eq!(
                Time::from_samples(rate * 3, rate as u32).to_samples(rate as u32),
                rate * 3
            );
        }
    }

    #[test]
    fn timebase_round_trip() {
        let t = Time::from_timebase(90_000 * 5, 1, 90_000);
        assert_eq!(t, Time::from_seconds(5.0));
        assert_eq!(t.to_timebase(1, 90_000), 450_000);
    }

    #[test]
    fn display() {
        assert_eq!(Time::from_seconds(3723.5).to_string(), "01:02:03.500");
        assert_eq!((-Time::from_seconds(1.25)).to_string(), "-00:00:01.250");
    }
}
