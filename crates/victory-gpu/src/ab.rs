//! Settings that change while the title runs, so that one run measures both
//! sides in the same scene (every build).
//!
//! - `VICTORY_AB=<seconds>,<switch>[,<switch>..]`: every so many seconds, at
//!   a present, the next phase begins; switch number n is on in the phases
//!   whose number has bit n, so that all combinations come one after another,
//!   again and again.
//! - `VICTORY_LIVE_ALT=<NAME>=<a>|<b>,<seconds>`: one setting, `a` in the
//!   even phases and `b` in the odd ones (a switch takes 0 and 1; a number of
//!   `crate::live` any two of its values). `VICTORY_AB` goes first when both
//!   are set.
//!
//! Each phase's frame times are logged at its end (the frames of its first
//! second left out), and the frame-rate line says the phase it was in
//! ([`tag`]). A setting named here does not go by its own variable (nor by
//! the live file, `crate::live`) any more.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// One setting that changes: its name, and for `VICTORY_LIVE_ALT` its two
/// values (a switch of `VICTORY_AB`: 0 and 1).
#[derive(Debug, PartialEq)]
struct Entry {
    name: String,
    values: Option<[f32; 2]>,
}

impl Entry {
    /// Its value in a phase where its bit is `set`.
    fn value(&self, set: bool) -> f32 {
        match self.values {
            Some([a, b]) => {
                if set {
                    b
                } else {
                    a
                }
            }
            None => f32::from(u8::from(set)),
        }
    }

    /// How the phase log and [`tag`] show it.
    fn shown(&self, set: bool) -> String {
        format!("{}={}", self.name, self.value(set))
    }
}

#[derive(Debug, PartialEq)]
struct Asked {
    seconds: f64,
    entries: Vec<Entry>,
}

/// `VICTORY_AB`'s value.
fn parse(text: &str) -> Option<Asked> {
    let mut parts = text.split(',').map(str::trim);
    let seconds = parts.next()?.parse::<f64>().ok().filter(|s| *s > 1.0)?;
    let entries: Vec<Entry> = parts
        .filter(|s| !s.is_empty())
        .map(|s| Entry {
            name: s.to_string(),
            values: None,
        })
        .collect();
    (!entries.is_empty() && entries.len() <= 8).then_some(Asked { seconds, entries })
}

/// `VICTORY_LIVE_ALT`'s value: `NAME=a|b,seconds`.
fn parse_alt(text: &str) -> Option<Asked> {
    let (setting, seconds) = text.trim().rsplit_once(',')?;
    let seconds = seconds
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 1.0)?;
    let (name, values) = setting.split_once('=')?;
    let name = name.trim();
    let (a, b) = values.split_once('|')?;
    let value = |v: &str| v.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    let values = [value(a)?, value(b)?];
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| Asked {
        seconds,
        entries: vec![Entry {
            name: name.to_string(),
            values: Some(values),
        }],
    })
}

fn asked() -> Option<&'static Asked> {
    static ASKED: OnceLock<Option<Asked>> = OnceLock::new();
    ASKED
        .get_or_init(|| {
            let ab = std::env::var("VICTORY_AB").ok();
            let alt = std::env::var("VICTORY_LIVE_ALT").ok();
            if let Some(text) = &ab {
                let asked = parse(text);
                if asked.is_none() {
                    tracing::warn!(text, "VICTORY_AB wants <seconds over 1>,<switch>[,<switch>..] (at most 8): not used");
                } else if alt.is_some() {
                    tracing::warn!("VICTORY_AB and VICTORY_LIVE_ALT are both set: VICTORY_LIVE_ALT is not used");
                }
                return asked;
            }
            let text = alt?;
            let asked = parse_alt(&text);
            match &asked {
                Some(a) => {
                    let e = &a.entries[0];
                    if !crate::live::known(&e.name) {
                        tracing::warn!(
                            setting = e.name,
                            "VICTORY_LIVE_ALT: not one of the live settings (crate::live); it changes only if its code asks \
                             crate::ab"
                        );
                    }
                    tracing::info!(
                        setting = e.name,
                        a = e.value(false),
                        b = e.value(true),
                        seconds = a.seconds,
                        "VICTORY_LIVE_ALT: the setting goes from one value to the other at a present"
                    );
                }
                None => tracing::warn!(text, "VICTORY_LIVE_ALT wants <NAME>=<a>|<b>,<seconds over 1>: not used"),
            }
            asked
        })
        .as_ref()
}

/// The settings that are on now (with two values: the second one), a bit each.
static BITS: AtomicU32 = AtomicU32::new(0);
/// The phase under way, plus one (0: none began).
static PHASE: AtomicU64 = AtomicU64::new(0);

/// Whether `switch` is on now, if it is one of those that change, or the
/// live file (`crate::live`) has it otherwise than the run's own.
pub(crate) fn pick(switch: &str) -> Option<bool> {
    value(switch)
        .map(|v| v != 0.0)
        .or_else(|| crate::live::switch_named(switch))
}

/// The value of `setting` now, if it is one of those that change.
pub(crate) fn value(setting: &str) -> Option<f32> {
    let asked = asked()?;
    let bit = asked.entries.iter().position(|e| e.name == setting)?;
    Some(asked.entries[bit].value(BITS.load(Relaxed) >> bit & 1 != 0))
}

/// The number of `switch` among those that change.
pub(crate) fn bit(switch: &str) -> Option<u32> {
    Some(asked()?.entries.iter().position(|e| e.name == switch)? as u32)
}

/// Whether the switch of that number is on now.
pub(crate) fn on(bit: u32) -> bool {
    let set = BITS.load(Relaxed) >> bit & 1 != 0;
    asked()
        .and_then(|a| a.entries.get(bit as usize))
        .map_or(set, |e| e.value(set) != 0.0)
}

/// For the frame-rate line: the phase under way and its values, if any.
pub(crate) fn tag() -> Option<String> {
    let asked = asked()?;
    let phase = PHASE.load(Relaxed).checked_sub(1)?;
    let bits = BITS.load(Relaxed);
    let values: Vec<String> = asked
        .entries
        .iter()
        .enumerate()
        .map(|(n, e)| e.shown(bits >> n & 1 != 0))
        .collect();
    Some(format!("A/B phase {phase} [{}]", values.join(" ")))
}

/// What the run has seen of the phase under way.
struct Phase {
    started: std::time::Instant,
    number: u64,
    began: std::time::Instant,
    last: std::time::Instant,
    drawn: u64,
    frames: Vec<f32>,
    draws: u64,
}

/// The frames' mean, median and nine tenths.
fn summary(frames: &mut [f32]) -> (f32, f32, f32) {
    if frames.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mean = frames.iter().sum::<f32>() / frames.len() as f32;
    frames.sort_by(f32::total_cmp);
    (
        mean,
        frames[frames.len() / 2],
        frames[frames.len() * 9 / 10],
    )
}

/// The title's present ends on this thread. `change`: called when another
/// phase begins, before any switch changes, with what `switch` is and what it
/// will be.
pub(crate) fn presented(change: impl Fn(&str, bool, bool)) {
    let Some(asked) = asked() else {
        return;
    };
    static NOW: Mutex<Option<Phase>> = Mutex::new(None);
    let now = std::time::Instant::now();
    let drawn = crate::draw::drawn();
    let mut phase = NOW.lock().unwrap_or_else(|e| e.into_inner());
    let p = phase.get_or_insert_with(|| {
        PHASE.store(1, Relaxed);
        Phase {
            started: now,
            number: 0,
            began: now,
            last: now,
            drawn,
            frames: Vec::new(),
            draws: 0,
        }
    });
    if (now - p.began).as_secs_f64() > 1.0 {
        p.frames.push((now - p.last).as_secs_f32() * 1000.0);
        p.draws += drawn.saturating_sub(p.drawn);
    }
    (p.last, p.drawn) = (now, drawn);
    let number = ((now - p.started).as_secs_f64() / asked.seconds) as u64;
    if number == p.number {
        return;
    }
    let bits = |number: u64| (number % (1 << asked.entries.len())) as u32;
    let (was, will) = (bits(p.number), bits(number));
    let (mean, median, most) = summary(&mut p.frames);
    let values: Vec<String> = asked
        .entries
        .iter()
        .enumerate()
        .map(|(n, e)| e.shown(was >> n & 1 != 0))
        .collect();
    tracing::info!(
        "A/B phase {} bits {was} [{}]: {} frames, mean {mean:.1} ms ({:.2} fps), median {median:.1}, p90 {most:.1}, {:.0} draws \
         a frame",
        p.number,
        values.join(" "),
        p.frames.len(),
        if mean > 0.0 { 1000.0 / mean } else { 0.0 },
        p.draws as f64 / p.frames.len().max(1) as f64
    );
    for (n, e) in asked.entries.iter().enumerate() {
        let (a, b) = (was >> n & 1 != 0, will >> n & 1 != 0);
        change(&e.name, e.value(a) != 0.0, e.value(b) != 0.0);
    }
    BITS.store(will, Relaxed);
    PHASE.store(number + 1, Relaxed);
    (p.number, p.began, p.draws) = (number, now, 0);
    p.frames.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_and_seconds_are_read_and_frames_summed_up() {
        let a = parse("10, VICTORY_A,VICTORY_B").unwrap();
        assert_eq!(a.seconds, 10.0);
        assert_eq!(
            a.entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["VICTORY_A", "VICTORY_B"]
        );
        assert!(a.entries.iter().all(|e| e.values.is_none()));
        assert!(parse("10").is_none());
        assert!(parse("x,VICTORY_A").is_none());
        assert!(parse("0,VICTORY_A").is_none());
        assert_eq!(summary(&mut [30.0, 10.0, 20.0]), (20.0, 20.0, 30.0));
        assert_eq!(summary(&mut []), (0.0, 0.0, 0.0));
        assert_eq!(pick("VICTORY_NONE_SUCH"), None);
    }

    #[test]
    fn an_alternation_is_read() {
        let a = parse_alt("VICTORY_UP_QUEUE=0|1,20").unwrap();
        assert_eq!(a.seconds, 20.0);
        assert_eq!(
            a.entries,
            vec![Entry {
                name: "VICTORY_UP_QUEUE".into(),
                values: Some([0.0, 1.0])
            }]
        );
        let a = parse_alt(" VICTORY_LAND_THREADS = 4 | 2 , 7.5 ").unwrap();
        assert_eq!(
            (a.seconds, &a.entries[0].name),
            (7.5, &"VICTORY_LAND_THREADS".to_string())
        );
        assert_eq!(
            (a.entries[0].value(false), a.entries[0].value(true)),
            (4.0, 2.0)
        );
        assert_eq!(a.entries[0].shown(true), "VICTORY_LAND_THREADS=2");
        // Phases need more than a second (its first is left out).
        assert!(parse_alt("VICTORY_UP_QUEUE=0|1,1").is_none());
        assert!(parse_alt("VICTORY_UP_QUEUE=0|1").is_none());
        assert!(parse_alt("VICTORY_UP_QUEUE=0,20").is_none());
        assert!(parse_alt("VICTORY_UP_QUEUE=0|x,20").is_none());
        assert!(parse_alt("VICTORY_UP_QUEUE=0|nan,20").is_none());
        assert!(parse_alt("=0|1,20").is_none());
        assert!(parse_alt("A B=0|1,20").is_none());
        assert!(parse_alt("VICTORY_UP_QUEUE=0|1,inf").is_none());
    }

    #[test]
    fn a_switch_of_ab_is_its_bit() {
        let e = Entry {
            name: "S".into(),
            values: None,
        };
        assert_eq!((e.value(false), e.value(true)), (0.0, 1.0));
        let e = Entry {
            name: "S".into(),
            values: Some([1.0, 0.0]),
        };
        assert_eq!((e.value(false), e.value(true)), (1.0, 0.0));
    }
}
