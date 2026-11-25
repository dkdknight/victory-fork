//! The graphics driver's errors, seen as the system records them.
//!
//! A card is not lost out of nothing. Every loss of runs 201, 204, 208 and
//! 224 has entries of the card's driver in the system's event log (NVIDIA:
//! provider `nvlddmkm`, event 153, "Error occurred on GPUID") from 2.7 to
//! 6.3 seconds BEFORE the device was removed, several of them; run 210 has
//! three and kept its device; the runs with no such entry lost nothing.
//! Nothing in the layer saw them: the first it knew was the device gone.
//!
//! Here the layer subscribes to that log. Each entry of a display driver
//! is written to our log when the system records it, with our own clock,
//! so it can be laid beside what the layer did in that moment; it is
//! counted; and the first one trips a switch ([`tripped`]) that the paths
//! under suspicion ask before they run (the draw windows of `crate::draw`
//! turn themselves off for the rest of the run). So a driver error costs a
//! slower frame from then on, and not the card.
//!
//! On by default; `VICTORY_DRIVER_WATCH=0` subscribes to nothing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use windows_sys::Win32::System::EventLog::{
    EVT_HANDLE, EVT_SUBSCRIBE_NOTIFY_ACTION, EvtSubscribe, EvtSubscribeActionDeliver, EvtSubscribeToFutureEvents,
};

/// Entries of a display driver seen since the start.
static ERRORS: AtomicU64 = AtomicU64::new(0);
/// Whether one was seen ([`tripped`]).
static TRIPPED: AtomicBool = AtomicBool::new(false);

/// Whether the card's driver has reported an error in this run: what could
/// have caused it stops for the rest of the run.
pub(crate) fn tripped() -> bool {
    TRIPPED.load(Relaxed)
}

/// Driver errors seen since the start.
pub(crate) fn errors() -> u64 {
    ERRORS.load(Relaxed)
}

/// An entry of a display driver was recorded; `what` is its text, as
/// [`entry_text`] gives it.
fn seen(what: &str) {
    let n = ERRORS.fetch_add(1, Relaxed) + 1;
    let first = !TRIPPED.swap(true, Relaxed);
    tracing::error!(
        n,
        "the graphics driver reported an error to the system's event log: {what}{}",
        if first {
            "; from now on the paths under suspicion are off for this run (draw windows clamp each record again)"
        } else {
            ""
        }
    );
}

unsafe extern "system" fn on_event(action: EVT_SUBSCRIBE_NOTIFY_ACTION, _context: *const core::ffi::c_void, event: EVT_HANDLE) -> u32 {
    if action == EvtSubscribeActionDeliver {
        // SAFETY: the event handle the subscription delivers, live for
        // the call.
        let text = unsafe { render(event) }.map_or_else(|| "(its text could not be read)".to_owned(), |xml| entry_text(&xml));
        seen(&text);
    }
    0
}

/// An event's XML (`EvtRenderEventXml`), or `None`.
///
/// # Safety
/// `event` must be a live event handle.
unsafe fn render(event: EVT_HANDLE) -> Option<String> {
    use windows_sys::Win32::System::EventLog::{EvtRender, EvtRenderEventXml};
    let mut used = 0u32;
    let mut count = 0u32;
    // SAFETY: a size query, then a buffer of that size (in bytes).
    unsafe {
        EvtRender(0, event, EvtRenderEventXml, 0, std::ptr::null_mut(), &mut used, &mut count);
        if used == 0 {
            return None;
        }
        let mut buffer = vec![0u16; (used as usize).div_ceil(2)];
        if EvtRender(0, event, EvtRenderEventXml, used, buffer.as_mut_ptr().cast(), &mut used, &mut count) == 0 {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}

/// What an entry says, from its XML: the provider, the event number, the
/// system time and the texts of its data ("Graphics SM Warp Exception on
/// (GPC 1, TPC 3, SM 1): MMU Fault", "Graphics Exception: ESR ..."; the
/// NVIDIA driver's own message templates are not installed for the event
/// viewer, so the data is all there is). Pure, for the tests.
pub(crate) fn entry_text(xml: &str) -> String {
    let attribute = |tag: &str, name: &str| -> Option<String> {
        let at = xml.find(&format!("<{tag}"))?;
        let rest = &xml[at..];
        let rest = &rest[..rest.find('>')?];
        let key = format!("{name}=");
        let v = &rest[rest.find(&key)? + key.len()..];
        let quote = v.chars().next()?;
        let v = &v[1..];
        Some(v[..v.find(quote)?].to_owned())
    };
    let element = |tag: &str| -> Option<String> {
        let at = xml.find(&format!("<{tag}"))?;
        let rest = &xml[at..];
        let rest = &rest[rest.find('>')? + 1..];
        Some(rest[..rest.find(&format!("</{tag}>"))?].to_owned())
    };
    let mut data = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<Data") {
        let after = &rest[at..];
        let Some(open) = after.find('>') else { break };
        if after[..open].ends_with('/') {
            rest = &after[open + 1..];
            continue;
        }
        let body = &after[open + 1..];
        let Some(close) = body.find("</Data>") else { break };
        let text = body[..close].trim();
        if !text.is_empty() && data.len() < 4 {
            data.push(text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">"));
        }
        rest = &body[close..];
    }
    format!(
        "{} event {} at {}: {}",
        attribute("Provider", "Name").unwrap_or_else(|| "?".into()),
        element("EventID").unwrap_or_else(|| "?".into()),
        attribute("TimeCreated", "SystemTime").unwrap_or_else(|| "?".into()),
        data.join(" | ")
    )
}

/// The display drivers' entries of the last `seconds` seconds in the
/// system's event log, newest first, at most `most` of them, each as
/// [`entry_text`] says it: for the report of a lost device, whatever
/// `VICTORY_DRIVER_WATCH` says. A shader's page fault ("MMU Fault" on
/// many SMs) and a timeout of the card read differently there; the device's
/// removal reason is `DXGI_ERROR_DEVICE_HUNG` either way on NVIDIA.
pub(crate) fn recent(seconds: u64, most: usize) -> Vec<String> {
    use windows_sys::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let channel = wide("System");
    let query = wide(&format!(
        "*[System[Provider[@Name='nvlddmkm' or @Name='amdkmdag' or @Name='amdwddmg' or @Name='igfx' or @Name='igfxn' \
         or @Name='Display'] and TimeCreated[timediff(@SystemTime) <= {}]]]",
        seconds * 1000
    ));
    let mut out = Vec::new();
    // SAFETY: valid strings; handles are ours and closed; `EvtNext` fills
    // at most the slots given.
    unsafe {
        let results = EvtQuery(0, channel.as_ptr(), query.as_ptr(), EvtQueryChannelPath | EvtQueryReverseDirection);
        if results == 0 {
            return vec![format!("the system's event log could not be read ({})", std::io::Error::last_os_error())];
        }
        let mut events = [0isize; 16];
        while out.len() < most {
            let mut returned = 0u32;
            if EvtNext(results, events.len() as u32, events.as_mut_ptr(), 1000, 0, &mut returned) == 0 || returned == 0 {
                break;
            }
            for &event in &events[..returned as usize] {
                if out.len() < most
                    && let Some(xml) = render(event)
                {
                    out.push(entry_text(&xml));
                }
                EvtClose(event);
            }
        }
        EvtClose(results);
    }
    out
}

/// Subscribes to the display drivers' entries in the system's event log,
/// once. Entries from before the call are not delivered.
pub(crate) fn start() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // (`VICTORY_DRIVER_WATCH=1`: off by default here, on in fxceless's tree.)
        if std::env::var_os("VICTORY_DRIVER_WATCH").is_none_or(|v| v != "1") {
            return;
        }
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let channel = wide("System");
        // The kernel drivers of the three vendors, and the system's own
        // "display driver stopped responding" source.
        let query = wide(
            "*[System[Provider[@Name='nvlddmkm' or @Name='amdkmdag' or @Name='amdwddmg' or @Name='igfx' or @Name='igfxn' \
             or @Name='Display']]]",
        );
        // SAFETY: valid strings for the call's length; the callback is a
        // plain function that touches only statics; the subscription is
        // kept for the life of the process.
        let subscription = unsafe {
            EvtSubscribe(
                0,
                std::ptr::null_mut(),
                channel.as_ptr(),
                query.as_ptr(),
                0,
                std::ptr::null(),
                Some(on_event),
                EvtSubscribeToFutureEvents,
            )
        };
        if subscription == 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "no subscription to the system's event log: the graphics driver's errors are not seen as they happen"
            );
        } else {
            tracing::info!(
                "the graphics driver's errors in the system's event log are watched: each is logged here as it is recorded, and \
                 the first turns the paths under suspicion off (VICTORY_DRIVER_WATCH=1)"
            );
        }
    });
}

/// [`recent`]'s entries said shortly, oldest first: entries that differ
/// only by the place on the chip ("on (GPC 1, TPC 3, SM 1)") or by the
/// error registers of each SM are one, with their count, and of the
/// "Graphics Exception: ESR" ones the last two registers (the same on
/// every SM of the owner's loss: they look like the faulting warp's
/// program address, so one shader) are kept. Pure, for the tests.
pub(crate) fn summary(entries: &[String]) -> String {
    let mut kinds: Vec<(String, String, usize)> = Vec::new();
    for e in entries.iter().rev() {
        let (head, data) = e.split_once(": ").unwrap_or((e.as_str(), ""));
        let (who, at) = head.split_once(" at ").unwrap_or((head, ""));
        let mut key = data.to_owned();
        if let Some(from) = key.find(" on (GPC")
            && let Some(len) = key[from..].find(')')
        {
            key.replace_range(from..from + len + 1, "");
        }
        if let Some(esr) = key.find("Graphics Exception: ESR") {
            let values: Vec<&str> = key[esr..].split_whitespace().filter_map(|t| t.split_once('=').map(|(_, v)| v)).collect();
            let tail = if values.len() >= 4 { format!(" (last two registers {} {})", values[2], values[3]) } else { String::new() };
            key = format!("{}Graphics Exception: ESR{tail}", &key[..esr]);
        }
        let key = format!("{who}: {key}");
        match kinds.iter_mut().find(|(k, _, _)| *k == key) {
            Some(kind) => kind.2 += 1,
            None => kinds.push((key, at.to_owned(), 1)),
        }
    }
    kinds
        .iter()
        .map(|(k, at, n)| if *n > 1 { format!("{k} x{n} (first at {at})") } else { format!("{k} (at {at})") })
        .collect::<Vec<_>>()
        .join(" || ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_error_trips_the_switch_and_every_one_is_counted() {
        // (Other tests do not touch these statics.)
        assert!(!tripped());
        assert_eq!(errors(), 0);
        seen("one");
        assert!(tripped());
        seen("two");
        assert_eq!(errors(), 2);
        assert!(tripped());
    }

    #[test]
    fn an_entry_says_its_provider_number_time_and_data() {
        let xml = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
            <Provider Name='nvlddmkm'/><EventID Qualifiers='49322'>13</EventID>\
            <TimeCreated SystemTime='2026-11-25T21:06:05.7120000Z'/></System><EventData>\
            <Data>\\Device\\00000272</Data><Data>Graphics SM Warp Exception on (GPC 1, TPC 3, SM 1): MMU Fault</Data>\
            <Data/><Binary>00000000</Binary></EventData></Event>";
        let text = entry_text(xml);
        assert_eq!(
            text,
            "nvlddmkm event 13 at 2026-11-25T21:06:05.7120000Z: \\Device\\00000272 | Graphics SM Warp Exception on (GPC 1, TPC 3, SM 1): MMU Fault"
        );
        assert_eq!(entry_text("<Event/>"), "? event ? at ?: ");
    }

    #[test]
    fn a_summary_makes_one_of_the_same_error_on_many_parts_of_the_chip() {
        let entries = [
            "nvlddmkm event 153 at T3: \\Device\\00000272 | Error occurred on GPUID: 100",
            "nvlddmkm event 13 at T2: \\Device\\00000272 | Graphics SM Warp Exception on (GPC 6, TPC 4, SM 0): MMU Fault",
            "nvlddmkm event 13 at T2: \\Device\\00000272 | Graphics Exception: ESR 0x569730=0x8000017 0x569734=0x4 0x569728=0x1c813b60 0x56972c=0x1104",
            "nvlddmkm event 13 at T1: \\Device\\00000272 | Graphics SM Warp Exception on (GPC 1, TPC 3, SM 1): MMU Fault",
            "nvlddmkm event 13 at T1: \\Device\\00000272 | Graphics Exception: ESR 0x5187b0=0x8030017 0x5187b4=0x4 0x5187a8=0x1c813b60 0x5187ac=0x1104",
            "nvlddmkm event 153 at T0: \\Device\\00000272 | Error occurred on GPUID: 100",
        ]
        .map(str::to_owned);
        assert_eq!(
            summary(&entries),
            "nvlddmkm event 153: \\Device\\00000272 | Error occurred on GPUID: 100 x2 (first at T0) || \
             nvlddmkm event 13: \\Device\\00000272 | Graphics Exception: ESR (last two registers 0x1c813b60 0x1104) x2 (first at T1) || \
             nvlddmkm event 13: \\Device\\00000272 | Graphics SM Warp Exception: MMU Fault x2 (first at T1)"
        );
        assert_eq!(summary(&[]), "");
    }

    /// By hand (`--ignored --nocapture`): the display driver's entries of
    /// the last day, as a lost device's report would say them.
    #[test]
    #[ignore = "prints what the system's event log holds: run it by name with --ignored"]
    fn print_the_last_days_entries() {
        for e in recent(86_400, 6) {
            eprintln!("{e}");
        }
    }

    /// The query of recent entries is one the system takes (whatever it
    /// finds).
    #[test]
    fn recent_entries_can_be_asked_for() {
        let found = recent(5, 4);
        assert!(found.iter().all(|e| !e.starts_with("the system's event log could not be read")), "{found:?}");
    }

    /// End to end, by hand (`--ignored`): an entry written to the
    /// Application log under a name of our own reaches a subscription made
    /// the same way as [`start`]'s, within a second. It leaves one
    /// information entry in that log.
    #[test]
    #[ignore = "writes one entry to the Application event log: run it by name with --ignored"]
    fn an_entry_written_reaches_a_subscription() {
        use windows_sys::Win32::System::EventLog::{
            DeregisterEventSource, EVENTLOG_INFORMATION_TYPE, EvtClose, RegisterEventSourceW, ReportEventW,
        };
        static GOT: AtomicU64 = AtomicU64::new(0);
        unsafe extern "system" fn got(action: EVT_SUBSCRIBE_NOTIFY_ACTION, _: *const core::ffi::c_void, _: EVT_HANDLE) -> u32 {
            if action == EvtSubscribeActionDeliver {
                GOT.fetch_add(1, Relaxed);
            }
            0
        }
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let (channel, query) = (wide("Application"), wide("*[System[Provider[@Name='victory-drvwatch-test']]]"));
        // SAFETY: as in `start`.
        let subscription = unsafe {
            EvtSubscribe(0, std::ptr::null_mut(), channel.as_ptr(), query.as_ptr(), 0, std::ptr::null(), Some(got), EvtSubscribeToFutureEvents)
        };
        assert_ne!(subscription, 0, "{}", std::io::Error::last_os_error());
        let (name, text) = (wide("victory-drvwatch-test"), wide("a test entry of the VIctory layer's driver watch"));
        // SAFETY: a source of our own name, one string, no data.
        unsafe {
            let source = RegisterEventSourceW(std::ptr::null(), name.as_ptr());
            assert!(!source.is_null(), "{}", std::io::Error::last_os_error());
            let strings = [text.as_ptr()];
            assert_ne!(
                ReportEventW(source, EVENTLOG_INFORMATION_TYPE, 0, 1, std::ptr::null_mut(), 1, 0, strings.as_ptr(), std::ptr::null()),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            DeregisterEventSource(source);
        }
        let started = std::time::Instant::now();
        while GOT.load(Relaxed) == 0 && started.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let after = started.elapsed();
        // SAFETY: our own subscription.
        unsafe { EvtClose(subscription) };
        assert_eq!(GOT.load(Relaxed), 1, "the entry was not delivered in 5 s");
        eprintln!("delivered after {} ms", after.as_millis());
    }

    /// The subscription itself: the system accepts the channel and the
    /// query (a query it cannot read is refused at once).
    #[test]
    fn the_system_takes_the_subscription() {
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let (channel, query) = (wide("System"), wide("*[System[Provider[@Name='nvlddmkm' or @Name='Display']]]"));
        // SAFETY: as in `start`.
        let subscription = unsafe {
            EvtSubscribe(0, std::ptr::null_mut(), channel.as_ptr(), query.as_ptr(), 0, std::ptr::null(), Some(on_event), EvtSubscribeToFutureEvents)
        };
        assert_ne!(subscription, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: our own subscription.
        unsafe { windows_sys::Win32::System::EventLog::EvtClose(subscription) };
    }
}
