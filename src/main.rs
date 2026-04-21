use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use std::fs;
use std::path::PathBuf;

const TARGET_TZ: &str = "America/Vancouver";
const AGGREGATE: &str = "aggregate.ics";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let downloads = dirs_home()?.join("Downloads");

    let ics_files: Vec<PathBuf> = fs::read_dir(&downloads)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("ics")
                && p.file_name().and_then(|s| s.to_str()) != Some(AGGREGATE)
        })
        .collect();

    if ics_files.is_empty() {
        eprintln!("No .ics files found in {}", downloads.display());
        return Ok(());
    }

    println!("Found {} file(s):", ics_files.len());
    for f in &ics_files {
        println!("  {}", f.display());
    }

    // Collect all VEVENT blocks, converting datetimes to Vancouver time.
    let mut all_events: Vec<String> = Vec::new();
    for path in &ics_files {
        let content = fs::read_to_string(path)?;
        let events = extract_and_convert_events(&content)?;
        all_events.extend(events);
    }

    // Build aggregate.ics.
    let aggregate_path = downloads.join(AGGREGATE);
    let mut output = String::new();
    output.push_str("BEGIN:VCALENDAR\r\n");
    output.push_str("VERSION:2.0\r\n");
    output.push_str("PRODID:-//downloadstzset//EN\r\n");
    output.push_str("CALSCALE:GREGORIAN\r\n");
    output.push_str(&vtimezone_vancouver());
    for event in &all_events {
        output.push_str(event);
    }
    output.push_str("END:VCALENDAR\r\n");

    fs::write(&aggregate_path, &output)?;
    println!("\nWrote {} event(s) to {}", all_events.len(), aggregate_path.display());

    // Send originals to Trash.
    println!("\nSending originals to Trash...");
    for path in &ics_files {
        trash::delete(path)?;
        println!("  Trashed: {}", path.display());
    }

    println!("\nDone.");
    Ok(())
}

/// Extract VEVENT blocks from calendar text, converting UTC DTSTART/DTEND to Vancouver time.
fn extract_and_convert_events(content: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut in_event = false;
    let mut current: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "BEGIN:VEVENT" {
            in_event = true;
            current.clear();
            current.push("BEGIN:VEVENT".to_string());
        } else if line == "END:VEVENT" {
            current.push("END:VEVENT".to_string());
            in_event = false;
            events.push(render_event(&current));
        } else if in_event {
            current.push(convert_line(line));
        }
    }

    Ok(events)
}

/// If the line is a UTC DTSTART or DTEND, convert it to Vancouver local time.
fn convert_line(line: &str) -> String {
    for prop in &["DTSTART", "DTEND"] {
        // Bare: DTSTART:20260421T224500Z
        let bare_prefix = format!("{}:", prop);
        if let Some(value) = line.strip_prefix(&bare_prefix) {
            if value.ends_with('Z') {
                if let Some(converted) = convert_utc_to_vancouver(value) {
                    return format!("{};TZID={}:{}", prop, TARGET_TZ, converted);
                }
            }
        }
        // With explicit UTC TZID: DTSTART;TZID=UTC:...
        let tzid_utc = format!("{};TZID=UTC:", prop);
        if let Some(value) = line.strip_prefix(&tzid_utc) {
            if let Some(converted) = convert_utc_to_vancouver(value) {
                return format!("{};TZID={}:{}", prop, TARGET_TZ, converted);
            }
        }
    }
    line.to_string()
}

/// Parse a UTC datetime string (e.g. "20260421T224500Z") and return Vancouver local form.
/// BC is on permanent DST (PDT = UTC-7).
fn convert_utc_to_vancouver(s: &str) -> Option<String> {
    let s = s.trim_end_matches('Z');
    let naive = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").ok()?;
    let pdt = FixedOffset::west_opt(7 * 3600)?;
    let local = pdt.from_utc_datetime(&naive);
    Some(local.format("%Y%m%dT%H%M%S").to_string())
}

/// Render a collected VEVENT block with CRLF line endings.
fn render_event(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// VTIMEZONE block for America/Vancouver on permanent PDT (UTC-7, no DST transitions).
fn vtimezone_vancouver() -> String {
    concat!(
        "BEGIN:VTIMEZONE\r\n",
        "TZID:America/Vancouver\r\n",
        "BEGIN:STANDARD\r\n",
        "DTSTART:19700101T000000\r\n",
        "TZOFFSETFROM:-0700\r\n",
        "TZOFFSETTO:-0700\r\n",
        "TZNAME:PDT\r\n",
        "END:STANDARD\r\n",
        "END:VTIMEZONE\r\n",
    )
    .to_string()
}

fn dirs_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME env var not set".into())
}
