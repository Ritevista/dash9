//! The duration value grammar shared by `range`, `refresh`, and the
//! dashboard TOML schema's duration fields. See `SPEC.md` B.4.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

impl DurationUnit {
    fn as_str(self) -> &'static str {
        match self {
            DurationUnit::Seconds => "s",
            DurationUnit::Minutes => "m",
            DurationUnit::Hours => "h",
            DurationUnit::Days => "d",
        }
    }
}

/// `<digits><unit>`, e.g. `30s`, `5m`, `1h`, `2d`. v0.1 supports
/// exactly one integer magnitude and one unit — no compound durations
/// like `1h30m`, no fractional magnitudes (SPEC.md B.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duration {
    pub magnitude: u64,
    pub unit: DurationUnit,
}

/// The duration string was empty, had no digits, or had an unknown
/// unit suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationParseError;

impl Duration {
    pub fn parse(s: &str) -> Result<Duration, DurationParseError> {
        let split_at = s
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map(|(idx, _)| idx)
            .ok_or(DurationParseError)?;
        let (digits, unit_str) = s.split_at(split_at);
        if digits.is_empty() {
            return Err(DurationParseError);
        }
        let magnitude: u64 = digits.parse().map_err(|_| DurationParseError)?;
        let unit = match unit_str {
            "s" => DurationUnit::Seconds,
            "m" => DurationUnit::Minutes,
            "h" => DurationUnit::Hours,
            "d" => DurationUnit::Days,
            _ => return Err(DurationParseError),
        };
        Ok(Duration { magnitude, unit })
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.magnitude, self.unit.as_str())
    }
}

/// `refresh` additionally accepts the literal `off` to disable
/// auto-refresh (SPEC.md B.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshInterval {
    Duration(Duration),
    Off,
}

impl RefreshInterval {
    pub fn parse(s: &str) -> Result<RefreshInterval, DurationParseError> {
        if s == "off" {
            return Ok(RefreshInterval::Off);
        }
        Duration::parse(s).map(RefreshInterval::Duration)
    }
}

impl fmt::Display for RefreshInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefreshInterval::Duration(d) => write!(f, "{d}"),
            RefreshInterval::Off => f.write_str("off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(
            Duration::parse("30s").unwrap(),
            Duration {
                magnitude: 30,
                unit: DurationUnit::Seconds
            }
        );
        assert_eq!(
            Duration::parse("5m").unwrap(),
            Duration {
                magnitude: 5,
                unit: DurationUnit::Minutes
            }
        );
        assert_eq!(
            Duration::parse("1h").unwrap(),
            Duration {
                magnitude: 1,
                unit: DurationUnit::Hours
            }
        );
        assert_eq!(
            Duration::parse("2d").unwrap(),
            Duration {
                magnitude: 2,
                unit: DurationUnit::Days
            }
        );
    }

    #[test]
    fn rejects_compound_and_fractional_and_unitless() {
        assert!(Duration::parse("1h30m").is_err());
        assert!(Duration::parse("1.5h").is_err());
        assert!(Duration::parse("30").is_err());
        assert!(Duration::parse("s").is_err());
        assert!(Duration::parse("").is_err());
        assert!(Duration::parse("30x").is_err());
    }

    #[test]
    fn refresh_interval_accepts_off() {
        assert_eq!(RefreshInterval::parse("off").unwrap(), RefreshInterval::Off);
        assert_eq!(
            RefreshInterval::parse("30s").unwrap(),
            RefreshInterval::Duration(Duration {
                magnitude: 30,
                unit: DurationUnit::Seconds
            })
        );
        assert!(RefreshInterval::parse("").is_err());
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in ["30s", "5m", "1h", "2d"] {
            let d = Duration::parse(s).unwrap();
            assert_eq!(d.to_string(), s);
        }
    }
}
