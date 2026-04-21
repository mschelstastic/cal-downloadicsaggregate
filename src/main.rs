use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::sync::mpsc;

const TARGET_TZ: &str = "America/Vancouver";
const AGGREGATE: &str = "aggregate.ics";
const AGGREGATE_TMP: &str = "aggregate.ics.tmp";
const SETTLE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let downloads = dirs_home()?.join("Downloads");

    // Files already present when we start are assumed complete.
    process_existing(&downloads)?;

    println!("Watching {} for new .ics files...", downloads.display());

    // Map of path -> time of last observed event, guarded by a mutex shared
    // between the watcher thread (writes) and the poller thread (reads/removes).
    let pending: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    // Poller: every POLL seconds, flush entries that haven't been touched in SETTLE seconds.
    let pending_poll = Arc::clone(&pending);
    let downloads_poll = downloads.clone();
    thread::spawn(move || loop {
        thread::sleep(POLL);
        let now = Instant::now();
        let ready: Vec<PathBuf> = {
            let mut map = pending_poll.lock().unwrap();
            let ready: Vec<PathBuf> = map
                .iter()
                .filter(|(_, t)| now.duration_since(**t) >= SETTLE)
                .map(|(p, _)| p.clone())
                .collect();
            for p in &ready {
                map.remove(p);
            }
            ready
        };
        for path in ready {
            if is_target_ics(&path) {
                println!("Processing (settled): {}", path.display());
                if let Err(e) = process_file(&downloads_poll, &path) {
                    eprintln!("Error processing {}: {}", path.display(), e);
                }
            }
        }
    });

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
    watcher.watch(&downloads, RecursiveMode::NonRecursive)?;

    for res in rx {
        match res {
            Ok(event) => {
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        | EventKind::Modify(notify::event::ModifyKind::Data(_))
                );
                if relevant {
                    let mut map = pending.lock().unwrap();
                    for path in event.paths {
                        if path.extension().and_then(|s| s.to_str()) == Some("ics")
                            && !matches!(
                                path.file_name().and_then(|s| s.to_str()),
                                Some(AGGREGATE) | Some(AGGREGATE_TMP)
                            )
                        {
                            map.insert(path, Instant::now());
                        }
                    }
                }
            }
            Err(e) => eprintln!("Watch error: {e}"),
        }
    }

    Ok(())
}

fn process_existing(downloads: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let ics_files: Vec<PathBuf> = fs::read_dir(downloads)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_target_ics(p))
        .collect();

    if ics_files.is_empty() {
        return Ok(());
    }

    println!("Processing {} existing file(s)...", ics_files.len());
    for path in ics_files {
        if let Err(e) = process_file(downloads, &path) {
            eprintln!("Error processing {}: {}", path.display(), e);
        }
    }
    Ok(())
}

fn process_file(downloads: &Path, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let new_events = extract_exercise_events(&content)?;

    if new_events.is_empty() {
        println!("  No exercise events in {}, leaving untouched.", path.display());
        return Ok(());
    }

    let count = new_events.len();
    println!("  {} exercise event(s) found.", count);
    update_aggregate(downloads, new_events)?;
    trash::delete(path)?;
    println!("  Trashed: {}", path.display());
    let title = path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown");
    macos_notify(
        "Exercise calendar updated",
        &format!("Added {} event{} from {}", count, if count == 1 { "" } else { "s" }, title),
    );
    Ok(())
}

fn update_aggregate(
    downloads: &Path,
    new_events: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let aggregate_path = downloads.join(AGGREGATE);
    let tmp_path = downloads.join(AGGREGATE_TMP);

    let mut events: Vec<String> = if aggregate_path.exists() {
        let content = fs::read_to_string(&aggregate_path)?;
        extract_raw_events(&content)
    } else {
        Vec::new()
    };

    events.extend(new_events);

    let mut output = String::new();
    output.push_str("BEGIN:VCALENDAR\r\n");
    output.push_str("VERSION:2.0\r\n");
    output.push_str("PRODID:-//downloadstzset//EN\r\n");
    output.push_str("CALSCALE:GREGORIAN\r\n");
    output.push_str(&vtimezone_vancouver());
    for event in &events {
        output.push_str(event);
    }
    output.push_str("END:VCALENDAR\r\n");

    // Write to tmp then rename for an atomic swap.
    fs::write(&tmp_path, &output)?;
    fs::rename(&tmp_path, &aggregate_path)?;

    println!(
        "  Wrote {} total event(s) to {}",
        events.len(),
        aggregate_path.display()
    );
    Ok(())
}

/// Extract VEVENT blocks from ICS, converting datetimes and filtering to exercise events only.
fn extract_exercise_events(content: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
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
            if is_exercise_event(&current) {
                events.push(render_event(&current));
            }
        } else if in_event {
            current.push(convert_line(line));
        }
    }

    Ok(events)
}

/// Extract raw VEVENT blocks from an already-processed aggregate (no datetime conversion).
fn extract_raw_events(content: &str) -> Vec<String> {
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
            current.push(line.to_string());
        }
    }

    events
}

fn is_exercise_event(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let lower = l.to_lowercase();
        lower.contains("onepeloton.com") || lower.contains("spinsociety.ca")
    })
}

fn convert_line(line: &str) -> String {
    for prop in &["DTSTART", "DTEND"] {
        let bare_prefix = format!("{}:", prop);
        if let Some(value) = line.strip_prefix(&bare_prefix) {
            if value.ends_with('Z') {
                if let Some(converted) = convert_utc_to_vancouver(value) {
                    return format!("{};TZID={}:{}", prop, TARGET_TZ, converted);
                }
            }
        }
        let tzid_utc = format!("{};TZID=UTC:", prop);
        if let Some(value) = line.strip_prefix(&tzid_utc) {
            if let Some(converted) = convert_utc_to_vancouver(value) {
                return format!("{};TZID={}:{}", prop, TARGET_TZ, converted);
            }
        }
    }
    line.to_string()
}

/// BC is on permanent DST (PDT = UTC-7).
fn convert_utc_to_vancouver(s: &str) -> Option<String> {
    let s = s.trim_end_matches('Z');
    let naive = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").ok()?;
    let pdt = FixedOffset::west_opt(7 * 3600)?;
    let local = pdt.from_utc_datetime(&naive);
    Some(local.format("%Y%m%dT%H%M%S").to_string())
}

fn render_event(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

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

fn is_target_ics(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("ics")
        && !matches!(
            path.file_name().and_then(|s| s.to_str()),
            Some(AGGREGATE) | Some(AGGREGATE_TMP)
        )
        && path.exists()
}

fn macos_notify(title: &str, message: &str) {
    // Escape for AppleScript string literals.
    let title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let message = message.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(r#"display notification "{message}" with title "{title}""#);
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status();
}

fn dirs_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME env var not set".into())
}
