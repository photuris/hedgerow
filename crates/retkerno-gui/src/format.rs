//! Pure display helpers: times, labels, glyphs, and colours.
//!
//! Local-time helpers take unix timestamps and use [`chrono::Local`], so the
//! results follow the machine's time zone.

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use retkerno_core::model::{DeliveryStatus, Message};

/// Seconds in a day.
const DAY_SECS: i64 = 86_400;

/// How far before `received_at` a sender clock may lie and still be trusted.
const SENDER_TS_PAST_SECS: i64 = 7 * DAY_SECS;

/// How far after `received_at` a sender clock may lie and still be trusted.
const SENDER_TS_FUTURE_SECS: i64 = 300;

/// Battery voltage that reads as 0 %.
const BATTERY_EMPTY_MV: u16 = 3300;

/// Battery voltage that reads as 100 %.
const BATTERY_FULL_MV: u16 = 4200;

/// `ts` as a local date and time (the epoch when out of range).
fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_default()
}

/// The local calendar day of `ts`.
pub fn local_date(ts: i64) -> NaiveDate {
    local(ts).date_naive()
}

/// Age of `ts` as seen at `now`: "now", "5m", "3h", a weekday within 7 days,
/// else a date such as "13 Oct".
pub fn relative_time(now: i64, ts: i64) -> String {
    let age = now - ts;

    if age < 60 {
        return "now".to_string();
    }

    if age < 3600 {
        return format!("{}m", age / 60);
    }

    if age < DAY_SECS {
        return format!("{}h", age / 3600);
    }

    if age < 7 * DAY_SECS {
        return local(ts).format("%a").to_string();
    }

    local(ts).format("%-d %b").to_string()
}

/// Day separator text: "Today", "Yesterday", else "Mon 13 Oct".
pub fn day_label(now: i64, ts: i64) -> String {
    let today = local_date(now);
    let day = local_date(ts);

    if day == today {
        return "Today".to_string();
    }

    if today.pred_opt() == Some(day) {
        return "Yesterday".to_string();
    }

    day.format("%a %-d %b").to_string()
}

/// Local wall-clock time of `ts` as `HH:MM`.
pub fn clock(ts: i64) -> String {
    local(ts).format("%H:%M").to_string()
}

/// The glyph shown beside an outgoing message's time.
pub fn status_glyph(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Pending => "◷",
        DeliveryStatus::Sent => "✓",
        DeliveryStatus::Delivered => "✓✓",
        DeliveryStatus::Failed => "!",
        DeliveryStatus::Received => "",
    }
}

/// Route length as text: "Flood" for -1, "Direct" for 0, else "n hop(s)".
pub fn path_label(path_len: i8) -> String {
    match path_len {
        ..0 => "Flood".to_string(),
        0 => "Direct".to_string(),
        1 => "1 hop".to_string(),
        n => format!("{n} hops"),
    }
}

/// A stable hue (0..360) for `name`, from a 32-bit FNV-1a hash.
pub fn name_hue(name: &str) -> u16 {
    let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });

    u16::try_from(hash % 360).unwrap_or(0)
}

/// Battery charge from its voltage: linear from 3300 mV (0 %) to 4200 mV
/// (100 %), clamped.
pub fn battery_percent(mv: u16) -> u8 {
    let span = u32::from(BATTERY_FULL_MV - BATTERY_EMPTY_MV);
    let above = u32::from(mv.clamp(BATTERY_EMPTY_MV, BATTERY_FULL_MV) - BATTERY_EMPTY_MV);

    u8::try_from((above * 100 + span / 2) / span).unwrap_or(100)
}

/// The time to show for `m`: the sender's clock when it is plausible
/// (within 7 days before to 300 s after `received_at`), else `received_at`.
pub fn display_ts(m: &Message) -> i64 {
    let sent = i64::from(m.sender_ts);
    let earliest = m.received_at - SENDER_TS_PAST_SECS;
    let latest = m.received_at + SENDER_TS_FUTURE_SECS;

    if (earliest..=latest).contains(&sent) {
        sent
    } else {
        m.received_at
    }
}

/// Hover text for a bubble: the sender's timestamp, SNR, and hop count
/// (the last two only when known).
pub fn hover_title(m: &Message) -> String {
    let sent = local(i64::from(m.sender_ts)).format("%Y-%m-%d %H:%M:%S");
    let mut title = format!("Sent {sent}");

    if let Some(snr) = m.snr {
        title.push_str(&format!(" · SNR {snr:.1} dB"));
    }

    if let Some(path) = m.path_len {
        title.push_str(&format!(
            " · {}",
            path_label(i8::try_from(path).unwrap_or(-1))
        ));
    }

    title
}

#[cfg(test)]
mod tests {
    use retkerno_core::model::{Direction, ThreadId};

    use super::*;

    /// Unix time of a local wall-clock moment, so tests hold in any zone.
    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    /// An incoming message with the given clocks.
    fn msg(sender_ts: u32, received_at: i64) -> Message {
        Message {
            id: 1,
            thread: ThreadId::Channel(0),
            direction: Direction::In,
            sender: None,
            text: String::new(),
            sender_ts,
            received_at,
            path_len: None,
            snr: None,
            status: DeliveryStatus::Received,
        }
    }

    mod relative_time {
        use super::*;

        const NOW: i64 = 1_760_000_000;

        #[test]
        fn should_say_now_under_a_minute() {
            assert_eq!(relative_time(NOW, NOW - 59), "now");
            assert_eq!(relative_time(NOW, NOW + 30), "now");
        }

        #[test]
        fn should_count_minutes_under_an_hour() {
            assert_eq!(relative_time(NOW, NOW - 60), "1m");
            assert_eq!(relative_time(NOW, NOW - 3599), "59m");
        }

        #[test]
        fn should_count_hours_under_a_day() {
            assert_eq!(relative_time(NOW, NOW - 3600), "1h");
            assert_eq!(relative_time(NOW, NOW - 86_399), "23h");
        }

        #[test]
        fn should_name_the_weekday_within_a_week() {
            let now = at(2026, 10, 14, 12, 0);

            assert_eq!(relative_time(now, at(2026, 10, 12, 12, 0)), "Mon");
            assert_eq!(relative_time(now, at(2026, 10, 8, 12, 0)), "Thu");
        }

        #[test]
        fn should_give_the_date_after_a_week() {
            let now = at(2026, 10, 14, 12, 0);

            assert_eq!(relative_time(now, at(2026, 10, 7, 11, 0)), "7 Oct");
            assert_eq!(relative_time(now, at(2026, 9, 3, 12, 0)), "3 Sep");
        }
    }

    mod day_label {
        use super::*;

        #[test]
        fn should_say_today_for_the_same_local_day() {
            let now = at(2026, 10, 14, 0, 30);

            assert_eq!(day_label(now, at(2026, 10, 14, 0, 5)), "Today");
        }

        #[test]
        fn should_say_yesterday_for_the_previous_local_day() {
            let now = at(2026, 10, 14, 0, 30);

            assert_eq!(day_label(now, at(2026, 10, 13, 23, 59)), "Yesterday");
        }

        #[test]
        fn should_give_weekday_and_date_otherwise() {
            let now = at(2026, 10, 14, 0, 30);

            assert_eq!(day_label(now, at(2026, 10, 12, 12, 0)), "Mon 12 Oct");
        }
    }

    #[test]
    fn clock_should_be_zero_padded_local_time() {
        assert_eq!(clock(at(2026, 10, 14, 9, 5)), "09:05");
        assert_eq!(clock(at(2026, 10, 14, 23, 41)), "23:41");
    }

    #[test]
    fn status_glyph_should_differ_per_status() {
        let glyphs = [
            (DeliveryStatus::Pending, "◷"),
            (DeliveryStatus::Sent, "✓"),
            (DeliveryStatus::Delivered, "✓✓"),
            (DeliveryStatus::Failed, "!"),
            (DeliveryStatus::Received, ""),
        ];

        for (status, glyph) in glyphs {
            assert_eq!(status_glyph(status), glyph, "{status:?}");
        }
    }

    #[test]
    fn path_label_should_name_flood_direct_and_hops() {
        assert_eq!(path_label(-1), "Flood");
        assert_eq!(path_label(0), "Direct");
        assert_eq!(path_label(1), "1 hop");
        assert_eq!(path_label(3), "3 hops");
    }

    mod name_hue {
        use super::*;

        #[test]
        fn should_match_known_fnv1a_values() {
            // FNV-1a/32: "" = 0x811c9dc5, "a" = 0xe40c292c.
            assert_eq!(name_hue(""), 61);
            assert_eq!(name_hue("a"), 340);
        }

        #[test]
        fn should_give_each_demo_name_its_own_hue() {
            // Computed independently of this crate, with FNV-1a/32 modulo 360.
            let expected = [("Alice", 143), ("Bob", 220), ("Carol", 210), ("Dave", 311)];

            for (name, hue) in expected {
                assert_eq!(name_hue(name), hue, "{name}");
            }
        }
    }

    mod battery_percent {
        use super::*;

        #[test]
        fn should_be_linear_between_empty_and_full() {
            assert_eq!(battery_percent(3300), 0);
            assert_eq!(battery_percent(3750), 50);
            assert_eq!(battery_percent(4200), 100);
        }

        #[test]
        fn should_clamp_outside_the_range() {
            assert_eq!(battery_percent(0), 0);
            assert_eq!(battery_percent(3000), 0);
            assert_eq!(battery_percent(4500), 100);
        }
    }

    mod display_ts {
        use super::*;

        const RECEIVED: i64 = 1_760_000_000;

        #[test]
        fn should_use_sender_ts_at_the_lower_bound() {
            let sent = u32::try_from(RECEIVED - 7 * 86_400).unwrap();

            assert_eq!(display_ts(&msg(sent, RECEIVED)), i64::from(sent));
        }

        #[test]
        fn should_use_received_at_below_the_lower_bound() {
            let sent = u32::try_from(RECEIVED - 7 * 86_400 - 1).unwrap();

            assert_eq!(display_ts(&msg(sent, RECEIVED)), RECEIVED);
        }

        #[test]
        fn should_use_sender_ts_at_the_upper_bound() {
            let sent = u32::try_from(RECEIVED + 300).unwrap();

            assert_eq!(display_ts(&msg(sent, RECEIVED)), i64::from(sent));
        }

        #[test]
        fn should_use_received_at_above_the_upper_bound() {
            let sent = u32::try_from(RECEIVED + 301).unwrap();

            assert_eq!(display_ts(&msg(sent, RECEIVED)), RECEIVED);
        }
    }

    mod hover_title {
        use super::*;

        #[test]
        fn should_show_sender_time_snr_and_hops_when_known() {
            let sent = u32::try_from(at(2026, 10, 14, 9, 5)).unwrap();
            let mut m = msg(sent, i64::from(sent));

            m.snr = Some(6.5);
            m.path_len = Some(2);

            assert_eq!(
                hover_title(&m),
                "Sent 2026-10-14 09:05:00 · SNR 6.5 dB · 2 hops"
            );
        }

        #[test]
        fn should_omit_snr_and_hops_when_unknown() {
            let sent = u32::try_from(at(2026, 10, 14, 9, 5)).unwrap();

            assert_eq!(
                hover_title(&msg(sent, i64::from(sent))),
                "Sent 2026-10-14 09:05:00"
            );
        }
    }
}
