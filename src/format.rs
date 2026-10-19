//! How times are written for viewers and in the app.

/// −0:10, −1:30, −1:02:00 (`sign` adds the leading minus)
pub fn clock(seconds: f64, sign: bool) -> String {
    let s = seconds.round().max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    let body = if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{sec:02}") };
    if sign { format!("−{body}") } else { body }
}

/// 5s, 90s, 2m: whole minutes in minutes, anything else in seconds (the shorter text).
/// This is how the on-stream badge shows the delay.
pub fn duration(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as u64;
    if s > 0 && s.is_multiple_of(60) { format!("{}m", s / 60) } else { format!("{s}s") }
}

/// 5s, 30s, 1m, 1m 30s
pub fn short(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let (m, rest) = (s / 60, s % 60);
    if rest > 0 { format!("{m}m {rest}s") } else { format!("{m}m") }
}

fn plural(n: u64, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// "10 seconds", "1 minute", "2 minutes 30 seconds"
pub fn spoken(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as u64;
    if s < 60 {
        return plural(s, "second");
    }
    let (m, rest) = (s / 60, s % 60);
    if rest > 0 { format!("{} {}", plural(m, "minute"), plural(rest, "second")) } else { plural(m, "minute") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_delays_like_a_clock() {
        assert_eq!(clock(10.0, true), "−0:10");
        assert_eq!(clock(75.0, false), "1:15");
        assert_eq!(clock(3723.0, true), "−1:02:03");
    }

    #[test]
    fn formats_delays_as_short_durations() {
        assert_eq!(duration(10.0), "10s");
        assert_eq!(duration(90.4), "90s");
        assert_eq!(duration(120.0), "2m");
        assert_eq!(duration(150.0), "150s");
        assert_eq!(short(90.0), "1m 30s");
        assert_eq!(short(300.0), "5m");
        assert_eq!(spoken(1.0), "1 second");
        assert_eq!(spoken(150.0), "2 minutes 30 seconds");
    }
}
