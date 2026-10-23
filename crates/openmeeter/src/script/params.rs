//! Voicemeeter-style parameters ("Strip[0].Mute", "Bus[1].Gain", "Recorder.play"),
//! as named by its Remote API, mapped onto OpenMeeter's mixer. Macro scripts read
//! and write these; Lua's `Strip`, `Bus`, ... tables are a view of them, and
//! `SetParameters` takes Voicemeeter's own script syntax.
//!
//! Strips and buses are numbered from 0 in mixer order, as in Voicemeeter:
//! hardware inputs, then virtual inputs; A buses, then B buses.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::model::{BusMode, KARAOKE_MODES, Kind, MAX_BUS_DELAY_MS, MAX_GAIN_DB, MIN_GAIN_DB, Mixer, RecordSource};

/// A parameter's value as a script sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Num(f64),
    Str(String),
}

/// What a script assigns: a number (or switch), text, or a pair like FadeTo's
/// `(-10, 500)`.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Num(f64),
    Str(String),
    Pair(f64, f64),
}

/// `=`, `+=` or `-=`. On a switch, `+= 1` flips it, as in Voicemeeter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Set,
    Add,
    Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Play,
    Pause,
    Stop,
    /// Play from the start.
    Replay,
    FastForward,
    Rewind,
    Record,
    StopRecording,
}

/// Something a script asked for that happens after it finishes, outside the mixer.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Restart,
    Shutdown,
    /// Show (true) or hide (false) the window.
    Show(bool),
    Eject,
    Reset,
    Save(PathBuf),
    Load(PathBuf),
    /// Run macro button `n` (0-based).
    PushButton(usize),
    Recorder(Transport),
    RecorderLoad(PathBuf),
    /// Jump to this many seconds into the cassette.
    Goto(f64),
    /// Move a strip or bus fader (by key) to `to` dB over `ms`.
    Fade { key: String, from: f32, to: f32, ms: f32 },
}

/// The recorder as a script sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecorderState {
    pub playing: bool,
    pub recording: bool,
    pub loaded: bool,
    /// Seconds into the cassette.
    pub position: f64,
}

/// Everything a script can see and change. The mixer is a copy: the app takes it
/// back, with the commands, once the script ends.
#[derive(Debug, Clone)]
pub struct Host {
    pub mixer: Mixer,
    /// The menu's Lock GUI option.
    pub locked: bool,
    pub recorder: RecorderState,
    /// How many macro buttons there are.
    pub buttons: usize,
    pub commands: Vec<Command>,
}

impl Host {
    pub fn new(mixer: Mixer, locked: bool, recorder: RecorderState, buttons: usize) -> Self {
        Self { mixer, locked, recorder, buttons, commands: Vec::new() }
    }
}

/// What a name refers to: a parameter, or a group of them ("Strip[0]", "Bus[1].mode").
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Value(Value),
    Group,
}

pub fn get(host: &mut Host, name: &str) -> Result<Lookup, String> {
    let segs = parse(name)?;
    let slot = slot(host, &parts(&segs)).map_err(|e| format!("{name}: {e}"))?;
    Ok(slot.map_or(Lookup::Group, |slot| Lookup::Value(read(&slot))))
}

pub fn set(host: &mut Host, name: &str, input: Input, op: Op) -> Result<(), String> {
    let segs = parse(name)?;
    let command = {
        let slot = slot(host, &parts(&segs)).map_err(|e| format!("{name}: {e}"))?;
        let slot = slot.ok_or_else(|| format!("{name} is a group of parameters, not one"))?;
        write(slot, input, op).map_err(|e| format!("{name}: {e}"))?
    };
    host.commands.extend(command);
    Ok(())
}

/// Run a Voicemeeter parameter script, e.g. `Strip[0].Mute = 1; Bus[0].Gain -= 3;`.
/// Statements end at `;` or a line break, `//` starts a comment, text goes in
/// quotes and pairs in brackets: `Strip[0].FadeTo = (-10, 500);`.
pub fn run(host: &mut Host, script: &str) -> Result<(), String> {
    for statement in statements(script) {
        let (name, op, value) = split_statement(&statement)?;
        set(host, name, parse_value(value), op)?;
    }
    Ok(())
}

// ---- names ----

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Name(String),
    Index(usize),
}

/// A name segment, borrowed so it can be matched against literals.
#[derive(Debug, Clone, Copy, PartialEq)]
enum P<'a> {
    N(&'a str),
    I(usize),
}

/// "Strip[0].A1", "strip(0).a1" and "Strip [0]. A1" all give strip, 0, a1:
/// Voicemeeter ignores case and spaces and accepts either kind of bracket.
fn parse(name: &str) -> Result<Vec<Seg>, String> {
    let bad = || format!("\"{name}\" isn't a parameter name");
    let mut segs = Vec::new();
    let mut word = String::new();
    let mut chars = name.chars().filter(|c| !c.is_whitespace());
    let flush = |word: &mut String, segs: &mut Vec<Seg>| {
        if !word.is_empty() {
            segs.push(Seg::Name(std::mem::take(word).to_ascii_lowercase()));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '.' => flush(&mut word, &mut segs),
            '[' | '(' => {
                flush(&mut word, &mut segs);
                let close = if c == '[' { ']' } else { ')' };
                let digits: String = chars.by_ref().take_while(|&d| d != close).collect();
                segs.push(Seg::Index(digits.parse().map_err(|_| bad())?));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => word.push(c),
            _ => return Err(bad()),
        }
    }
    flush(&mut word, &mut segs);
    if segs.is_empty() { Err(bad()) } else { Ok(segs) }
}

fn parts(segs: &[Seg]) -> Vec<P<'_>> {
    segs.iter()
        .map(|s| match s {
            Seg::Name(n) => P::N(n),
            Seg::Index(i) => P::I(*i),
        })
        .collect()
}

// ---- parameters ----

/// One parameter, borrowed from wherever it lives.
enum Slot<'a> {
    Flag(&'a mut bool),
    /// A number shown to scripts as `value + offset`, kept within `min..=max`.
    Number { value: &'a mut f32, min: f32, max: f32, offset: f32 },
    Karaoke(&'a mut u8),
    Channels(&'a mut u16),
    Text(&'a mut String),
    /// Whether `item` is in `set`: routes, playback buses, recorder arming.
    Member { set: &'a mut BTreeSet<String>, item: String },
    Mode { mode: &'a mut BusMode, this: BusMode },
    /// Voicemeeter's bus Mono: 0 off, 1 mono, 2 stereo reverse.
    BusMono { mono: &'a mut bool, reverse: &'a mut bool },
    RecordBuses(&'a mut RecordSource),
    ReadOnly(Value),
    /// A button: 1 does `on`, 0 does `off` (if anything); reads as `state`.
    Button { on: Command, off: Option<Command>, state: bool },
    /// Write-only text naming a file.
    File(fn(PathBuf) -> Command),
    Fade { key: String, current: f32, relative: bool },
    Goto(f64),
}

/// The parameter `p` names; `None` if it names a group.
fn slot<'a>(host: &'a mut Host, p: &[P]) -> Result<Option<Slot<'a>>, String> {
    use P::{I, N};
    let unknown = || Err("no such parameter".to_string());
    let bus_keys: Vec<String> = host.mixer.buses.iter().map(|b| b.key.clone()).collect();
    let bus_named = |name: &str| bus_keys.iter().find(|k| k.eq_ignore_ascii_case(name)).cloned();
    let Host { mixer, locked, recorder, buttons, .. } = host;
    let gain = |value| Slot::Number { value, min: MIN_GAIN_DB, max: MAX_GAIN_DB, offset: 0.0 };
    let knob = |value| Slot::Number { value, min: 0.0, max: 10.0, offset: 0.0 };
    let eq = |value| Slot::Number { value, min: -12.0, max: 12.0, offset: 0.0 };

    let slot = match p {
        [N("strip")] | [N("bus")] | [N("command")] | [N("recorder")] | [N("option")] => None,
        [N("strip"), I(i), rest @ ..] => {
            let count = mixer.strips.len();
            let strip = mixer.strips.get_mut(*i).ok_or_else(|| out_of_range("Strip", *i, count))?;
            let virt = strip.kind == Kind::Virtual;
            match rest {
                [] => None,
                [N("mute")] => Some(Slot::Flag(&mut strip.mute)),
                [N("solo")] => Some(Slot::Flag(&mut strip.solo)),
                [N("mono")] => Some(Slot::Flag(&mut strip.mono)),
                [N("mc")] => Some(Slot::Flag(&mut strip.mix_centre)),
                [N("gain")] => Some(gain(&mut strip.gain_db)),
                [N("label")] => Some(Slot::Text(&mut strip.label)),
                [N("comp")] => Some(knob(&mut strip.comp)),
                [N("gate")] => Some(knob(&mut strip.gate)),
                [N("karaoke")] => Some(Slot::Karaoke(&mut strip.karaoke)),
                [N("eqgain1")] => Some(eq(&mut strip.eq_bass)),
                [N("eqgain2")] => Some(eq(&mut strip.eq_mid)),
                [N("eqgain3")] => Some(eq(&mut strip.eq_treble)),
                [N("pan_x")] => {
                    let value = if virt { &mut strip.surround[0] } else { &mut strip.pan[0] };
                    Some(Slot::Number { value, min: -0.5, max: 0.5, offset: -0.5 })
                }
                [N("pan_y")] if virt => Some(Slot::Number { value: &mut strip.surround[1], min: -0.5, max: 0.5, offset: -0.5 }),
                [N("pan_y")] => Some(Slot::Number { value: &mut strip.pan[1], min: 0.0, max: 1.0, offset: 0.0 }),
                [N(fade @ ("fadeto" | "fadeby"))] => Some(Slot::Fade { key: strip.key.clone(), current: strip.gain_db, relative: *fade == "fadeby" }),
                [N(name)] => match bus_named(name) {
                    Some(item) => Some(Slot::Member { set: &mut strip.routes, item }),
                    None if is_bus_name(name) => return Err(format!("there is no bus {} in this layout", name.to_ascii_uppercase())),
                    None => return unknown(),
                },
                _ => return unknown(),
            }
        }
        [N("bus"), I(i), rest @ ..] => {
            let count = mixer.buses.len();
            let bus = mixer.buses.get_mut(*i).ok_or_else(|| out_of_range("Bus", *i, count))?;
            match rest {
                [] | [N("eq")] | [N("mode")] => None,
                [N("mute")] => Some(Slot::Flag(&mut bus.mute)),
                [N("mono")] => Some(Slot::BusMono { mono: &mut bus.mono, reverse: &mut bus.reverse }),
                [N("gain")] => Some(gain(&mut bus.gain_db)),
                [N("label")] => Some(Slot::ReadOnly(Value::Str(bus.key.clone()))),
                [N("eq"), N("on")] => Some(Slot::Flag(&mut bus.eq)),
                [N("mode"), N(name)] => {
                    let this = bus_mode(name).ok_or("no such bus mode")?;
                    Some(Slot::Mode { mode: &mut bus.mode, this })
                }
                [N(fade @ ("fadeto" | "fadeby"))] => Some(Slot::Fade { key: bus.key.clone(), current: bus.gain_db, relative: *fade == "fadeby" }),
                _ => return unknown(),
            }
        }
        [N("command"), rest @ ..] => match rest {
            [N("restart")] => Some(Slot::Button { on: Command::Restart, off: None, state: false }),
            [N("shutdown")] => Some(Slot::Button { on: Command::Shutdown, off: None, state: false }),
            [N("show")] => Some(Slot::Button { on: Command::Show(true), off: Some(Command::Show(false)), state: false }),
            [N("eject")] => Some(Slot::Button { on: Command::Eject, off: None, state: false }),
            [N("reset")] => Some(Slot::Button { on: Command::Reset, off: None, state: false }),
            [N("save")] => Some(Slot::File(Command::Save)),
            [N("load")] => Some(Slot::File(Command::Load)),
            [N("lock")] => Some(Slot::Flag(locked)),
            [N("button")] => None,
            [N("button"), I(i), rest @ ..] => {
                if *i >= *buttons {
                    return Err(out_of_range("Command.Button", *i, *buttons));
                }
                match rest {
                    [] => None,
                    [N("state")] => Some(Slot::Button { on: Command::PushButton(*i), off: None, state: false }),
                    _ => return unknown(),
                }
            }
            _ => return unknown(),
        },
        [N("recorder"), rest @ ..] => {
            let RecorderState { playing, recording, loaded, position } = *recorder;
            let transport = |on, off: Option<Transport>, state| Slot::Button { on: Command::Recorder(on), off: off.map(Command::Recorder), state };
            let settings = &mut mixer.recorder;
            match rest {
                [N("play")] => Some(transport(Transport::Play, Some(Transport::Pause), playing)),
                [N("pause")] => Some(transport(Transport::Pause, Some(Transport::Play), loaded && !playing && position > 0.0)),
                [N("stop")] => Some(transport(Transport::Stop, None, !playing && !recording)),
                [N("replay")] => Some(transport(Transport::Replay, None, false)),
                [N("ff")] => Some(transport(Transport::FastForward, None, false)),
                [N("rew")] => Some(transport(Transport::Rewind, None, false)),
                [N("record")] => Some(transport(Transport::Record, Some(Transport::StopRecording), recording)),
                [N("load")] => Some(Slot::File(Command::RecorderLoad)),
                [N("goto")] => Some(Slot::Goto(position)),
                [N("position")] => Some(Slot::ReadOnly(Value::Num(round(position)))),
                [N("gain")] => Some(gain(&mut settings.playback_gain_db)),
                [N("channel")] => Some(Slot::Channels(&mut settings.channels)),
                [N("mode")] | [N("armstrip")] | [N("armbus")] => None,
                [N("mode"), N("playonload")] => Some(Slot::Flag(&mut settings.play_on_load)),
                [N("mode"), N("loop")] => Some(Slot::Flag(&mut settings.loop_playback)),
                [N("mode"), N("multitrack")] => Some(Slot::Flag(&mut settings.multitrack)),
                [N("mode"), N("recbus")] => Some(Slot::RecordBuses(&mut settings.source)),
                [N("armstrip"), I(i)] => {
                    let key = mixer.strips.get(*i).map(|s| s.key.clone()).ok_or_else(|| out_of_range("Strip", *i, mixer.strips.len()))?;
                    Some(Slot::Member { set: &mut settings.armed_inputs, item: key })
                }
                [N("armbus"), I(i)] => {
                    let key = bus_keys.get(*i).cloned().ok_or_else(|| out_of_range("Bus", *i, bus_keys.len()))?;
                    Some(Slot::Member { set: &mut settings.record_buses, item: key })
                }
                [N(name)] => match bus_named(name) {
                    Some(item) => Some(Slot::Member { set: &mut settings.playback_buses, item }),
                    None if is_bus_name(name) => return Err(format!("there is no bus {} in this layout", name.to_ascii_uppercase())),
                    None => return unknown(),
                },
                _ => return unknown(),
            }
        }
        // Voicemeeter keeps the bus delays among its options.
        [N("option"), N("delay")] => None,
        [N("option"), N("delay"), I(i)] => {
            let count = mixer.buses.len();
            let bus = mixer.buses.get_mut(*i).ok_or_else(|| out_of_range("Bus", *i, count))?;
            Some(Slot::Number { value: &mut bus.delay_ms, min: 0.0, max: MAX_BUS_DELAY_MS, offset: 0.0 })
        }
        _ => return unknown(),
    };
    Ok(slot)
}

fn out_of_range(what: &str, i: usize, count: usize) -> String {
    match count {
        0 => format!("there is no {what}[{i}]: there are none"),
        _ => format!("there is no {what}[{i}] ({what}[0] to {what}[{}])", count - 1),
    }
}

/// A1..A8 or B1..B8, whether or not this layout has it.
fn is_bus_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a' | 'b')) && chars.as_str().parse::<u8>().is_ok()
}

fn bus_mode(name: &str) -> Option<BusMode> {
    Some(match name {
        "normal" => BusMode::Normal,
        "amix" => BusMode::MixDownA,
        "bmix" => BusMode::MixDownB,
        "repeat" => BusMode::StereoRepeat,
        "composite" => BusMode::Composite,
        "tvmix" => BusMode::TvMix,
        "upmix21" => BusMode::UpMix21,
        "upmix41" => BusMode::UpMix41,
        "upmix61" => BusMode::UpMix61,
        "centeronly" => BusMode::CenterOnly,
        "lfeonly" => BusMode::LfeOnly,
        "rearonly" => BusMode::RearOnly,
        _ => return None,
    })
}

/// f32 settings read back without float noise (0.1, not 0.10000000149).
fn round(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn read(slot: &Slot) -> Value {
    let flag = |on: bool| Value::Int(on.into());
    match slot {
        Slot::Flag(b) => flag(**b),
        Slot::Number { value, offset, .. } => Value::Num(round(f64::from(**value + offset))),
        Slot::Karaoke(k) => Value::Int((**k).into()),
        Slot::Channels(c) => Value::Int((**c).into()),
        Slot::Text(t) => Value::Str(t.to_string()),
        Slot::Member { set, item } => flag(set.contains(item)),
        Slot::Mode { mode, this } => flag(**mode == *this),
        Slot::BusMono { mono, reverse } => Value::Int(if **reverse { 2 } else { (**mono).into() }),
        Slot::RecordBuses(source) => flag(**source == RecordSource::PostFaderOutputs),
        Slot::ReadOnly(v) => v.clone(),
        Slot::Button { state, .. } => flag(*state),
        Slot::File(_) => Value::Str(String::new()),
        Slot::Fade { current, .. } => Value::Num(round(f64::from(*current))),
        Slot::Goto(position) => Value::Num(round(*position)),
    }
}

fn write(slot: Slot, input: Input, op: Op) -> Result<Option<Command>, String> {
    let number = || match &input {
        Input::Num(n) => Ok(*n),
        Input::Str(s) => s.trim().parse().map_err(|_| format!("wants a number, not \"{s}\"")),
        Input::Pair(..) => Err("wants a number, not a pair".to_string()),
    };
    // Switches: `= 0` / `= 1` set them, `+= 1` flips them.
    let switch = |current: bool| -> Result<bool, String> {
        let n = number()?;
        Ok(match op {
            Op::Set => n != 0.0,
            Op::Add | Op::Sub => current ^ (n != 0.0),
        })
    };
    let relative = |current: f64| -> Result<f64, String> {
        let n = number()?;
        Ok(match op {
            Op::Set => n,
            Op::Add => current + n,
            Op::Sub => current - n,
        })
    };
    match slot {
        Slot::Flag(b) => *b = switch(*b)?,
        Slot::Number { value, min, max, offset } => {
            let target = relative(f64::from(*value + offset))? as f32;
            *value = target.clamp(min, max) - offset;
        }
        Slot::Karaoke(k) => *k = relative(f64::from(*k))?.round().clamp(0.0, f64::from(KARAOKE_MODES)) as u8,
        Slot::Channels(c) => *c = relative(f64::from(*c))?.round().clamp(1.0, 2.0) as u16,
        Slot::Text(t) => {
            *t = match input {
                Input::Str(s) => s,
                Input::Num(n) => n.to_string(),
                Input::Pair(..) => return Err("wants text, not a pair".into()),
            }
        }
        Slot::Member { set, item } => {
            if switch(set.contains(&item))? {
                set.insert(item);
            } else {
                set.remove(&item);
            }
        }
        Slot::Mode { mode, this } => {
            if switch(*mode == this)? {
                *mode = this;
            } else if *mode == this {
                *mode = BusMode::Normal;
            }
        }
        Slot::BusMono { mono, reverse } => {
            let current = if *reverse { 2.0 } else { f64::from(u8::from(*mono)) };
            // `+= 1` steps through off, mono, reverse like the button does.
            let next = match op {
                Op::Set => number()?.round(),
                Op::Add => (current + number()?.round()).rem_euclid(3.0),
                Op::Sub => (current - number()?.round()).rem_euclid(3.0),
            };
            (*mono, *reverse) = match next as i64 {
                0 => (false, false),
                1 => (true, false),
                2 => (false, true),
                _ => return Err("wants 0 (off), 1 (mono) or 2 (stereo reverse)".into()),
            };
        }
        Slot::RecordBuses(source) => {
            let buses = switch(*source == RecordSource::PostFaderOutputs)?;
            *source = if buses { RecordSource::PostFaderOutputs } else { RecordSource::PreFaderInputs };
        }
        Slot::ReadOnly(_) => return Err("is read-only".into()),
        Slot::Button { on, off, state } => return Ok(if switch(state)? { Some(on) } else { off }),
        Slot::File(command) => {
            return match input {
                Input::Str(path) if !path.trim().is_empty() => Ok(Some(command(PathBuf::from(path.trim())))),
                _ => Err("wants a file name in quotes".into()),
            };
        }
        Slot::Fade { key, current, relative } => {
            let (db, ms) = match input {
                Input::Pair(db, ms) => (db, ms),
                Input::Str(s) => pair(&s).ok_or("wants (dB, milliseconds), e.g. (-10, 500)")?,
                Input::Num(_) => return Err("wants (dB, milliseconds), e.g. (-10, 500)".into()),
            };
            let target = if relative { f64::from(current) + db } else { db };
            let to = (target as f32).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            return Ok(Some(Command::Fade { key, from: current, to, ms: ms.clamp(0.0, 120_000.0) as f32 }));
        }
        Slot::Goto(position) => {
            let seconds = match &input {
                Input::Str(s) if s.contains(':') => clock_time(s).ok_or("wants seconds or hh:mm:ss")?,
                _ => relative(position)?,
            };
            return Ok(Some(Command::Goto(seconds.max(0.0))));
        }
    }
    Ok(None)
}

/// "(-10, 500)" or "-10, 500".
fn pair(text: &str) -> Option<(f64, f64)> {
    let text = text.trim().trim_start_matches('(').trim_end_matches(')');
    let (a, b) = text.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// "hh:mm:ss" or "mm:ss" in seconds.
fn clock_time(text: &str) -> Option<f64> {
    text.trim().split(':').try_fold(0.0, |total, part| part.trim().parse::<f64>().ok().map(|n| total * 60.0 + n))
}

// ---- Voicemeeter script syntax ----

/// Statements, split at `;` and line breaks outside quotes and brackets, with
/// `//` comments removed.
fn statements(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut depth) = (false, 0usize);
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            '/' if !quoted && chars.peek() == Some(&'/') => {
                while chars.next_if(|&c| c != '\n').is_some() {}
                continue;
            }
            ';' | '\n' if !quoted && (depth == 0 || c == '\n') => {
                out.push(std::mem::take(&mut current));
                depth = 0;
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    out.push(current);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn split_statement(statement: &str) -> Result<(&str, Op, &str), String> {
    let eq = statement.find('=').ok_or_else(|| format!("\"{statement}\" has no = in it"))?;
    let (name, op) = match statement[..eq].strip_suffix('+') {
        Some(name) => (name, Op::Add),
        None => match statement[..eq].strip_suffix('-') {
            Some(name) => (name, Op::Sub),
            None => (&statement[..eq], Op::Set),
        },
    };
    Ok((name.trim(), op, statement[eq + 1..].trim()))
}

fn parse_value(text: &str) -> Input {
    if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return Input::Str(inner.to_string());
    }
    if text.starts_with('(')
        && let Some((a, b)) = pair(text)
    {
        return Input::Pair(a, b);
    }
    match text.parse() {
        Ok(n) => Input::Num(n),
        Err(_) => Input::Str(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(Mixer::default(), false, RecorderState::default(), 2)
    }

    fn value(host: &mut Host, name: &str) -> Value {
        match get(host, name).unwrap() {
            Lookup::Value(v) => v,
            Lookup::Group => panic!("{name} is a group"),
        }
    }

    #[test]
    fn names_are_read_like_voicemeeter_reads_them() {
        let mut h = host();
        h.mixer.strips[1].mute = true;
        for name in ["Strip[1].Mute", "strip(1).mute", "Strip [1] . Mute"] {
            assert_eq!(value(&mut h, name), Value::Int(1), "{name}");
        }
        assert_eq!(get(&mut h, "Strip[1]"), Ok(Lookup::Group));
        assert_eq!(get(&mut h, "Bus[0].mode"), Ok(Lookup::Group));
        assert_eq!(value(&mut h, "Bus[3].Label"), Value::Str("B1".into()), "buses run A1-A3 then B1-B2");
        assert!(get(&mut h, "Strip[9].Mute").unwrap_err().contains("Strip[0] to Strip[4]"));
        assert!(get(&mut h, "Strip[0].Wobble").is_err());
        assert!(get(&mut h, "Strip[0].A5").unwrap_err().contains("no bus A5"));
    }

    #[test]
    fn writes_change_the_mixer() {
        let mut h = host();
        set(&mut h, "Strip[0].Gain", Input::Num(-6.0), Op::Set).unwrap();
        set(&mut h, "Strip[0].Gain", Input::Num(2.5), Op::Add).unwrap();
        assert_eq!(h.mixer.strips[0].gain_db, -3.5);
        set(&mut h, "Strip[0].Gain", Input::Num(100.0), Op::Set).unwrap();
        assert_eq!(h.mixer.strips[0].gain_db, MAX_GAIN_DB, "clamped to the fader's range");

        set(&mut h, "Strip[0].B1", Input::Num(1.0), Op::Set).unwrap();
        assert!(h.mixer.strips[0].routes.contains("B1"));
        set(&mut h, "Strip[0].A1", Input::Num(1.0), Op::Add).unwrap();
        assert!(!h.mixer.strips[0].routes.contains("A1"), "+= 1 flips a switch");

        set(&mut h, "Strip[2].Pan_x", Input::Num(0.5), Op::Set).unwrap();
        assert_eq!(h.mixer.strips[2].pan[0], 1.0, "Voicemeeter's -0.5..0.5 is the pad's 0..1");
        set(&mut h, "Strip[0].Label", Input::Str("Mic".into()), Op::Set).unwrap();
        assert_eq!(h.mixer.strips[0].label, "Mic");

        set(&mut h, "Bus[0].Mono", Input::Num(2.0), Op::Set).unwrap();
        assert!(h.mixer.buses[0].reverse && !h.mixer.buses[0].mono);
        set(&mut h, "Bus[0].Mono", Input::Num(1.0), Op::Add).unwrap();
        assert!(!h.mixer.buses[0].reverse && !h.mixer.buses[0].mono, "steps round to off");
        set(&mut h, "Bus[1].mode.TVMix", Input::Num(1.0), Op::Set).unwrap();
        assert_eq!(h.mixer.buses[1].mode, BusMode::TvMix);
        assert!(set(&mut h, "Bus[0].Label", Input::Str("x".into()), Op::Set).is_err());

        set(&mut h, "Recorder.B2", Input::Num(1.0), Op::Set).unwrap();
        assert!(h.mixer.recorder.playback_buses.contains("B2"));
        set(&mut h, "Recorder.ArmStrip[3]", Input::Num(1.0), Op::Set).unwrap();
        assert!(h.mixer.recorder.armed_inputs.contains("v1"));
        set(&mut h, "Option.delay[0]", Input::Num(20.0), Op::Set).unwrap();
        assert_eq!(h.mixer.buses[0].delay_ms, 20.0);
        set(&mut h, "Command.Lock", Input::Num(1.0), Op::Set).unwrap();
        assert!(h.locked);
        assert!(h.commands.is_empty(), "all of that is mixer state");
    }

    #[test]
    fn commands_wait_for_the_app() {
        let mut h = host();
        h.mixer.buses[0].gain_db = -4.0;
        set(&mut h, "Command.Restart", Input::Num(1.0), Op::Set).unwrap();
        set(&mut h, "Recorder.play", Input::Num(1.0), Op::Set).unwrap();
        set(&mut h, "Recorder.play", Input::Num(0.0), Op::Set).unwrap();
        set(&mut h, "Recorder.load", Input::Str("C:/a.wav".into()), Op::Set).unwrap();
        set(&mut h, "Recorder.goto", Input::Str("00:01:30".into()), Op::Set).unwrap();
        set(&mut h, "Bus[0].FadeBy", Input::Pair(-6.0, 500.0), Op::Set).unwrap();
        set(&mut h, "Command.Button[1].State", Input::Num(1.0), Op::Set).unwrap();
        assert_eq!(
            h.commands,
            [
                Command::Restart,
                Command::Recorder(Transport::Play),
                Command::Recorder(Transport::Pause),
                Command::RecorderLoad("C:/a.wav".into()),
                Command::Goto(90.0),
                Command::Fade { key: "A1".into(), from: -4.0, to: -10.0, ms: 500.0 },
                Command::PushButton(1),
            ]
        );
        assert!(set(&mut h, "Command.Button[2].State", Input::Num(1.0), Op::Set).is_err(), "only two buttons");
    }

    #[test]
    fn voicemeeter_scripts_run() {
        let mut h = host();
        let script = "Strip[0].Mute = 1; Strip[1].Gain -= 3 // quieter\nStrip(2).Label = \"Game, chat\";\n\
                      Bus[0].FadeTo = (-10, 250); Strip[0].A1 += 1";
        run(&mut h, script).unwrap();
        assert!(h.mixer.strips[0].mute);
        assert_eq!(h.mixer.strips[1].gain_db, -3.0);
        assert_eq!(h.mixer.strips[2].label, "Game, chat");
        assert!(!h.mixer.strips[0].routes.contains("A1"));
        assert_eq!(h.commands, [Command::Fade { key: "A1".into(), from: 0.0, to: -10.0, ms: 250.0 }]);
        assert!(run(&mut h, "Strip[0].Mute").is_err());
        assert!(run(&mut h, "Strip[0].Bogus = 1").unwrap_err().contains("Strip[0].Bogus"));
    }
}
