//! Parse instants without repairing invalid clock or calendar fields.
use crate::scan::Root;
use chrono::{DateTime, Utc};

pub fn parse(value: &str, root: Root) -> Option<DateTime<Utc>> {
    let value = value.trim();
    // Atom requires RFC 3339 with uppercase T and Z (RFC 4287 section 3.3).
    // RSS 1.0 uses W3CDTF; RSS 2.0 feeds also commonly use RFC 3339.
    let rfc3339 = || {
        if root == Root::Atom && (value.as_bytes().get(10) != Some(&b'T') || value.ends_with('z')) {
            return None;
        }
        DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    };
    if root != Root::Rss {
        return rfc3339();
    }
    rfc3339().or_else(|| {
        // Keep the existing tolerance of a mismatched weekday in RSS feeds. It does
        // not change the date's fields, unlike feed-rs's repair of 24:xx to 00:xx.
        let uncommented = without_comments(value)?;
        let value = uncommented.trim();
        let value = value.split_once(',').map_or(value, |(_, date)| date.trim());
        let zone = value.split_ascii_whitespace().last()?;
        let normalized;
        let value = if zone.eq_ignore_ascii_case("UTC") || zone.eq_ignore_ascii_case("Z") {
            normalized = format!("{}+0000", &value[..value.len() - zone.len()]);
            &normalized
        } else {
            value
        };
        DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    })
}

// RFC 5322 comments may follow the zone or occur between date fields. Remove
// them before normalizing the UTC spelling, including `UTC (comment)`.
fn without_comments(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0;
    let mut escaped = false;
    for c in value.chars() {
        if depth > 0 {
            if escaped {
                escaped = false;
            } else {
                match c {
                    '\\' => escaped = true,
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
            }
        } else if c == '(' {
            depth = 1;
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    (depth == 0).then_some(out)
}
