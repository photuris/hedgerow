//! Pure display helpers: times, labels, glyphs, and colours.
//!
//! Local-time helpers take unix timestamps and use [`chrono::Local`], so the
//! results follow the machine's time zone.

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use hedgerow_core::model::{DeliveryStatus, Message};

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

/// Sender label lightness in light and dark mode (percent) and chroma.
const NAME_LIGHT: (u8, f64) = (48, 0.08);
const NAME_DARK: (u8, f64) = (78, 0.11);

/// The bubble tint color (percent lightness, chroma) and how much of it
/// mixes into the raised surface (percent).
const TINT: (u8, f64) = (70, 0.11);
const TINT_MIX: u8 = 20;

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

/// The inline style for an incoming channel bubble sent by `name`.
///
/// `--name` colors the sender label, and `--tint` is the bubble background.
/// Both come from the hue of [`name_hue`]: `--name` switches between a dark
/// and a light variant with `light-dark`, and `--tint` mixes a little of the
/// hue into the raised surface, so it follows the theme. The contrast test in
/// this module checks [`NAME_LIGHT`], [`NAME_DARK`], [`TINT`] and
/// [`TINT_MIX`] against the supported themes.
pub fn name_style(name: &str) -> String {
    let hue = name_hue(name);
    let (light_l, light_c) = NAME_LIGHT;
    let (dark_l, dark_c) = NAME_DARK;
    let (tint_l, tint_c) = TINT;

    format!(
        "--name: light-dark(oklch({light_l}% {light_c:.2} {hue}), oklch({dark_l}% {dark_c:.2} {hue})); \
         --tint: color-mix(in oklab, oklch({tint_l}% {tint_c:.2} {hue}) {TINT_MIX}%, var(--surface-raised))"
    )
}

/// The label of the shortcut `Ctrl+key`, or `⌘key` on macOS.
pub fn chord(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
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

/// Splits `text` into consecutive pieces, flagging the ones that match
/// `query`. The pieces always add up to `text`.
///
/// The match ignores ASCII case only, like the store's `LIKE`, so every hit
/// the store reports is found here. Pieces are cut on character boundaries,
/// so any UTF-8 text is safe.
pub fn highlight<'a>(text: &'a str, query: &str) -> Vec<(&'a str, bool)> {
    let width = query.len();
    let mut pieces = Vec::new();
    let (mut plain_from, mut at) = (0, 0);

    while width > 0 && at < text.len() {
        if let Some(hit) = text.get(at..at + width)
            && hit.eq_ignore_ascii_case(query)
        {
            if plain_from < at {
                pieces.push((&text[plain_from..at], false));
            }

            pieces.push((hit, true));
            at += width;
            plain_from = at;
        } else {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
    }

    if plain_from < text.len() {
        pieces.push((&text[plain_from..], false));
    }

    pieces
}

#[cfg(test)]
mod tests {
    use hedgerow_core::model::{Direction, ThreadId};

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

    mod name_style {
        use super::*;

        /// A color as three channels, in whatever space the caller says.
        type Rgb = [f64; 3];

        /// Decodes a gamma-encoded sRGB channel to linear light.
        fn decode(c: f64) -> f64 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }

        /// A `#rrggbb` color as gamma-encoded channels in 0..=1.
        fn hex(color: &str) -> Rgb {
            let channel =
                |at: usize| f64::from(u8::from_str_radix(&color[at..at + 2], 16).unwrap()) / 255.0;

            [channel(1), channel(3), channel(5)]
        }

        /// Linear-light channels of gamma-encoded ones.
        fn linear(encoded: Rgb) -> Rgb {
            encoded.map(decode)
        }

        /// Linear sRGB to OKLab (Ottosson).
        fn to_oklab([r, g, b]: Rgb) -> Rgb {
            let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
            let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
            let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();

            [
                0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
                1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
                0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
            ]
        }

        /// OKLab to linear sRGB (Ottosson), without any clamping.
        fn from_oklab([lightness, a, b]: Rgb) -> Rgb {
            let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
            let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
            let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);

            [
                4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
                -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
                -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
            ]
        }

        /// `oklch(lightness% chroma hue)` as OKLab.
        fn oklch((lightness, chroma): (u8, f64), hue: u16) -> Rgb {
            let radians = f64::from(hue).to_radians();

            [
                f64::from(lightness) / 100.0,
                chroma * radians.cos(),
                chroma * radians.sin(),
            ]
        }

        /// Whether every linear channel is inside sRGB, within rounding.
        fn in_gamut(linear: Rgb) -> bool {
            linear.iter().all(|c| (-1e-6..=1.0 + 1e-6).contains(c))
        }

        /// WCAG relative luminance of linear channels, clamped to sRGB.
        fn luminance(linear: Rgb) -> f64 {
            let [r, g, b] = linear.map(|c| c.clamp(0.0, 1.0));

            0.2126 * r + 0.7152 * g + 0.0722 * b
        }

        /// WCAG contrast ratio of two linear colors.
        fn contrast(a: Rgb, b: Rgb) -> f64 {
            let (a, b) = (luminance(a), luminance(b));

            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }

        /// A fixture theme: name, dark mode, background, foreground and the
        /// raised surface, with `None` meaning the stylesheet default.
        type Theme = (
            &'static str,
            bool,
            &'static str,
            &'static str,
            Option<&'static str>,
        );

        const THEMES: [Theme; 6] = [
            ("default-light", false, "#ffffff", "#000000", None),
            ("default-dark", true, "#1a1a1a", "#ffffff", None),
            ("omarchy-dark", true, "#1A1915", "#ECE9E1", Some("#211F1A")),
            ("latte", false, "#EFF1F5", "#4C4F69", Some("#E6E9EF")),
            ("dawn", false, "#FAF4ED", "#575279", Some("#F2E9E1")),
            ("gruvbox", true, "#282828", "#D4BE98", Some("#32302F")),
        ];

        #[test]
        fn should_render_the_alice_style() {
            assert_eq!(
                name_style("Alice"),
                "--name: light-dark(oklch(48% 0.08 143), oklch(78% 0.11 143)); --tint: color-mix(in oklab, oklch(70% 0.11 143) 20%, var(--surface-raised))"
            );
        }

        #[test]
        fn should_keep_text_readable_on_every_theme() {
            for (name, dark, bg, fg, raised) in THEMES {
                let (bg, fg) = (hex(bg), hex(fg));
                // `color-mix(in srgb, fg 5%, bg)` mixes the encoded channels.
                let raised =
                    raised.map_or_else(|| [0, 1, 2].map(|i| 0.05 * fg[i] + 0.95 * bg[i]), hex);
                let raised = to_oklab(linear(raised));
                let label = if dark { NAME_DARK } else { NAME_LIGHT };

                for hue in 0..360 {
                    let name_color = from_oklab(oklch(label, hue));
                    let tint = from_oklab(oklch(TINT, hue));

                    assert!(
                        in_gamut(name_color) && in_gamut(tint),
                        "{name} hue {hue}: name {name_color:?} or tint {tint:?} outside sRGB"
                    );

                    let ratio = contrast(name_color, linear(bg));

                    assert!(
                        ratio >= 4.5,
                        "{name} hue {hue}: name on background is {ratio:.2}"
                    );

                    let share = f64::from(TINT_MIX) / 100.0;
                    let tint = to_oklab(tint);
                    let bubble =
                        from_oklab([0, 1, 2].map(|i| share * tint[i] + (1.0 - share) * raised[i]));
                    let ratio = contrast(linear(fg), bubble);

                    assert!(
                        ratio >= 4.5,
                        "{name} hue {hue}: text on bubble is {ratio:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn chord_should_name_the_platform_modifier() {
        if cfg!(target_os = "macos") {
            assert_eq!(chord("B"), "⌘B");
        } else {
            assert_eq!(chord("B"), "Ctrl+B");
            assert_eq!(chord(","), "Ctrl+,");
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

    mod highlight {
        use super::*;

        #[test]
        fn should_flag_every_match_ignoring_ascii_case() {
            assert_eq!(
                highlight("Net NET net", "net"),
                [
                    ("Net", true),
                    (" ", false),
                    ("NET", true),
                    (" ", false),
                    ("net", true)
                ]
            );
        }

        #[test]
        fn should_keep_the_texts_own_case_in_the_hit() {
            assert_eq!(
                highlight("Loud and clear", "AND"),
                [("Loud ", false), ("and", true), (" clear", false)]
            );
        }

        #[test]
        fn should_cut_on_character_boundaries_in_utf8_text() {
            assert_eq!(
                highlight("café ☕ café", "é"),
                [("caf", false), ("é", true), (" ☕ caf", false), ("é", true)]
            );
            assert_eq!(
                highlight("日本語のテスト", "のテ"),
                [("日本語", false), ("のテ", true), ("スト", false)]
            );
        }

        #[test]
        fn should_not_match_inside_a_multibyte_character() {
            // 'é' is 0xC3 0xA9; a query of the single byte-width ASCII "ab" must
            // never be compared against a slice that splits it.
            assert_eq!(highlight("éé", "ab"), [("éé", false)]);
        }

        #[test]
        fn should_return_the_text_whole_when_nothing_matches() {
            assert_eq!(highlight("hello", "xyz"), [("hello", false)]);
            assert_eq!(highlight("hi", "longer than the text"), [("hi", false)]);
        }

        #[test]
        fn should_return_the_text_whole_for_an_empty_query() {
            assert_eq!(highlight("hello", ""), [("hello", false)]);
        }

        #[test]
        fn should_return_nothing_for_empty_text() {
            assert!(highlight("", "a").is_empty());
        }

        #[test]
        fn should_add_up_to_the_original_text() {
            let text = "aXbxXc";
            let joined: String = highlight(text, "x")
                .iter()
                .map(|(piece, _)| *piece)
                .collect();

            assert_eq!(joined, text);
        }
    }
}
