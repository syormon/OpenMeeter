//! Completions and inline help for the script editor: what can follow the text
//! before the cursor (`Strip[0].` → Mute, Gain, A1...), and what the parameter
//! under the cursor is (`Strip[0].Gain`: "fader, -60 to +12 dB", now -6).
//!
//! The parameter lists here mirror `params`; a test checks every one resolves.

/// Something that can be typed at the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// What the list shows.
    pub label: String,
    /// What replaces the partial word. Ends in `.` or `[` for groups, so the
    /// list carries on with what's inside.
    pub insert: String,
    pub detail: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Parameter,
    Group,
    Index,
    Function,
    Keyword,
    Variable,
}

/// Completions for the text before `cursor` (a byte offset): the items, and
/// where the word they replace starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub start: usize,
    pub items: Vec<Item>,
}

/// The mixer's names, for the lists that depend on the layout.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// Strip labels, in order.
    pub strips: Vec<String>,
    /// Bus keys, in order ("A1", ..., "B1", ...).
    pub buses: Vec<String>,
    /// Macro button names, in order.
    pub buttons: Vec<String>,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil", "not", "or", "repeat",
    "return", "then", "true", "until", "while",
];

/// Top-level names besides keywords: (name, insert, detail, kind).
const GLOBALS: &[(&str, &str, &str, Kind)] = &[
    ("Strip", "Strip[", "input strips, from 0: Strip[0] is the first", Kind::Group),
    ("Bus", "Bus[", "output buses, from 0: Bus[0] is A1", Kind::Group),
    ("Recorder", "Recorder.", "the cassette recorder", Kind::Group),
    ("Command", "Command.", "app commands: restart, load, show...", Kind::Group),
    ("Option", "Option.", "options (bus delays)", Kind::Group),
    ("GetParameter", "GetParameter(", "GetParameter(\"Strip[0].Gain\"): read a parameter by name", Kind::Function),
    ("SetParameter", "SetParameter(", "SetParameter(\"Strip[0].Gain\", -6): set a parameter by name", Kind::Function),
    ("SetParameters", "SetParameters(", "SetParameters(\"Strip[0].Gain += 3; Bus[0].Mute = 1\"): Voicemeeter script syntax", Kind::Function),
    ("print", "print(", "print(...): to the output pane and the log", Kind::Function),
    ("require", "require(", "require(\"name\"): run name.lua from the scripts folder, return its result", Kind::Function),
    ("math", "math.", "maths library", Kind::Group),
    ("string", "string.", "string library", Kind::Group),
    ("table", "table.", "table library", Kind::Group),
    ("os", "os.", "time and date", Kind::Group),
    ("utf8", "utf8.", "UTF-8 library", Kind::Group),
    ("tostring", "tostring(", "tostring(v)", Kind::Function),
    ("tonumber", "tonumber(", "tonumber(v [, base])", Kind::Function),
    ("type", "type(", "type(v): \"nil\", \"number\", \"string\", \"table\"...", Kind::Function),
    ("pairs", "pairs(", "pairs(t): iterate a table's keys and values", Kind::Function),
    ("ipairs", "ipairs(", "ipairs(t): iterate a list", Kind::Function),
    ("select", "select(", "select(n, ...)", Kind::Function),
    ("error", "error(", "error(message): stop with an error", Kind::Function),
    ("assert", "assert(", "assert(v [, message])", Kind::Function),
    ("pcall", "pcall(", "pcall(f, ...): call f, catching errors", Kind::Function),
    ("next", "next(", "next(t [, key])", Kind::Function),
    ("setmetatable", "setmetatable(", "setmetatable(t, mt)", Kind::Function),
    ("getmetatable", "getmetatable(", "getmetatable(t)", Kind::Function),
    ("rawget", "rawget(", "rawget(t, k)", Kind::Function),
    ("rawset", "rawset(", "rawset(t, k, v)", Kind::Function),
];

const LIBRARIES: &[(&str, &[(&str, &str)])] = &[
    (
        "math",
        &[
            ("abs", "abs(x)"),
            ("ceil", "ceil(x)"),
            ("floor", "floor(x)"),
            ("max", "max(x, ...)"),
            ("min", "min(x, ...)"),
            ("random", "random([m [, n]])"),
            ("randomseed", "randomseed(x)"),
            ("sqrt", "sqrt(x)"),
            ("fmod", "fmod(x, y)"),
            ("modf", "modf(x)"),
            ("exp", "exp(x)"),
            ("log", "log(x [, base])"),
            ("sin", "sin(x)"),
            ("cos", "cos(x)"),
            ("tan", "tan(x)"),
            ("tointeger", "tointeger(x)"),
            ("type", "type(x): \"integer\", \"float\" or nil"),
            ("huge", "infinity"),
            ("pi", "3.14159..."),
            ("maxinteger", "largest integer"),
            ("mininteger", "smallest integer"),
        ],
    ),
    (
        "string",
        &[
            ("format", "format(fmt, ...): like C's printf"),
            ("len", "len(s)"),
            ("lower", "lower(s)"),
            ("upper", "upper(s)"),
            ("sub", "sub(s, i [, j])"),
            ("rep", "rep(s, n [, sep])"),
            ("reverse", "reverse(s)"),
            ("find", "find(s, pattern [, init [, plain]])"),
            ("match", "match(s, pattern [, init])"),
            ("gmatch", "gmatch(s, pattern)"),
            ("gsub", "gsub(s, pattern, repl [, n])"),
            ("byte", "byte(s [, i [, j]])"),
            ("char", "char(...)"),
        ],
    ),
    (
        "table",
        &[
            ("insert", "insert(t, [pos,] value)"),
            ("remove", "remove(t [, pos])"),
            ("concat", "concat(t [, sep [, i [, j]]])"),
            ("sort", "sort(t [, comp])"),
            ("unpack", "unpack(t [, i [, j]])"),
            ("pack", "pack(...)"),
        ],
    ),
    ("os", &[("time", "time(): seconds since 1970"), ("clock", "clock(): seconds since OpenMeeter started"), ("date", "date([format]): e.g. os.date(\"%H:%M\")")]),
    (
        "utf8",
        &[("char", "char(...)"), ("codepoint", "codepoint(s [, i [, j]])"), ("len", "len(s)"), ("offset", "offset(s, n [, i])"), ("codes", "codes(s)")],
    ),
];

const STRIP: &[(&str, &str)] = &[
    ("Mute", "mute switch, 0 or 1"),
    ("Solo", "solo switch, 0 or 1"),
    ("Mono", "mono switch, 0 or 1"),
    ("MC", "mix-to-centre switch (virtual inputs)"),
    ("Gain", "fader, -60 to +12 dB"),
    ("Label", "the strip's name (text)"),
    ("Comp", "compressor knob, 0 to 10"),
    ("Gate", "gate knob, 0 to 10"),
    ("Karaoke", "karaoke mode, 0 (off) to 4"),
    ("EQGain1", "bass, -12 to +12 dB (virtual inputs)"),
    ("EQGain2", "mid, -12 to +12 dB (virtual inputs)"),
    ("EQGain3", "treble, -12 to +12 dB (virtual inputs)"),
    ("Pan_x", "pan left/right, -0.5 to 0.5"),
    ("Pan_y", "pan pad up/down, 0 to 1 (virtual inputs: front/rear, -0.5 to 0.5)"),
    ("FadeTo", "= \"(dB, ms)\" or {dB, ms}: fade the fader to dB over ms"),
    ("FadeBy", "= \"(dB, ms)\" or {dB, ms}: fade the fader by dB over ms"),
];

const BUS: &[(&str, &str)] = &[
    ("Mute", "mute switch, 0 or 1"),
    ("Mono", "0 off, 1 mono, 2 stereo reverse"),
    ("Gain", "fader, -60 to +12 dB"),
    ("Label", "the bus name, \"A1\"... (read only)"),
    ("FadeTo", "= \"(dB, ms)\" or {dB, ms}: fade the fader to dB over ms"),
    ("FadeBy", "= \"(dB, ms)\" or {dB, ms}: fade the fader by dB over ms"),
];

const BUS_MODES: &[(&str, &str)] = &[
    ("normal", "Normal mode"),
    ("Amix", "MIX down A"),
    ("Bmix", "MIX down B"),
    ("Repeat", "Stereo Repeat"),
    ("Composite", "Composite"),
    ("TVMix", "TV Mix"),
    ("UpMix21", "UpMix 2.1"),
    ("UpMix41", "UpMix 4.1"),
    ("UpMix61", "UpMix 6.1"),
    ("CenterOnly", "Center only"),
    ("LFEOnly", "LFE only"),
    ("RearOnly", "Rear only"),
];

const COMMAND: &[(&str, &str)] = &[
    ("Restart", "= 1: restart the audio engine"),
    ("Show", "= 1: show the window, = 0: hide it"),
    ("Shutdown", "= 1: quit OpenMeeter"),
    ("Eject", "= 1: eject the cassette"),
    ("Reset", "= 1: reset the whole mixer"),
    ("Load", "= \"C:/path/settings.json\": load settings"),
    ("Save", "= \"C:/path/settings.json\": save settings"),
    ("Lock", "Lock GUI switch, 0 or 1"),
];

const RECORDER: &[(&str, &str)] = &[
    ("play", "= 1: play, = 0: pause; reads 1 while playing"),
    ("pause", "= 1: pause"),
    ("stop", "= 1: stop and rewind (ends a recording)"),
    ("record", "= 1: start recording, = 0: stop; reads 1 while recording"),
    ("replay", "= 1: play from the start"),
    ("ff", "= 1: skip forward 10 s"),
    ("rew", "= 1: skip back 10 s"),
    ("load", "= \"C:/clips/horn.wav\": load a file"),
    ("goto", "= \"00:01:30\" or seconds: jump in the cassette"),
    ("position", "seconds into the cassette (read only)"),
    ("gain", "playback level, -60 to +12 dB"),
    ("channel", "record 1 (mono) or 2 (stereo) channels"),
];

const RECORDER_MODES: &[(&str, &str)] = &[
    ("recbus", "1: record buses (post-fader), 0: record inputs (pre-fader)"),
    ("PlayOnLoad", "play as soon as a file is loaded"),
    ("Loop", "loop playback"),
    ("MultiTrack", "one file per armed source"),
];

/// What to suggest at `cursor` in `text`, or `None` when nothing fits there (in a
/// comment or string, or after something that isn't a name).
pub fn complete(text: &str, cursor: usize, names: &Names) -> Option<Completion> {
    let before = text.get(..cursor)?;
    if in_comment_or_string(before) {
        return None;
    }
    let partial_len = before.chars().rev().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').map(char::len_utf8).sum::<usize>();
    let start = cursor - partial_len;
    let partial = &before[start..];
    let rest = &before[..start];

    let mut items = if let Some(object) = rest.strip_suffix('.') {
        let path = resolve(&expression_before(object)?, text, start);
        members(&path, names)
    } else if let Some(object) = rest.strip_suffix('[') {
        let path = resolve(&expression_before(object)?, text, start);
        indices(&path, names)
    } else {
        // A digit-led word is a number, and `obj:` or `a.b` endings aren't names we know.
        if partial.starts_with(|c: char| c.is_ascii_digit()) || rest.ends_with(':') {
            return None;
        }
        globals(text, start)
    };

    let lower = partial.to_ascii_lowercase();
    items.retain(|item| item.label.to_ascii_lowercase().starts_with(&lower) && !item.label.eq_ignore_ascii_case(partial));
    (!items.is_empty()).then_some(Completion { start, items })
}

/// The parameter at the cursor and what it does, e.g. ("Strip[0].Gain", "fader,
/// -60 to +12 dB"), for the help line under the editor.
pub fn describe(text: &str, cursor: usize, names: &Names) -> Option<(String, String)> {
    let before = text.get(..cursor)?;
    let after = &text[cursor..];
    let word_end = cursor + after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').map(char::len_utf8).sum::<usize>();
    let expression = expression_before(&text[..word_end])?;
    if in_comment_or_string(before) {
        return None;
    }
    let path = resolve(&expression, text, cursor);
    let (object, field) = match path.rfind(['.', '[']) {
        Some(i) if path[i..].starts_with('.') => (&path[..i], &path[i + 1..]),
        _ => ("", path.as_str()),
    };
    let detail = if object.is_empty() {
        globals(text, usize::MAX).into_iter().find(|i| i.label == field && i.kind != Kind::Variable && i.kind != Kind::Keyword)?.detail
    } else {
        members(object, names).into_iter().find(|i| i.label.eq_ignore_ascii_case(field))?.detail
    };
    Some((path, detail))
}

/// Inside a `--` comment or an unfinished string on the cursor's line.
fn in_comment_or_string(before: &str) -> bool {
    let line = before.rsplit('\n').next().unwrap_or(before);
    let mut quote = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(_) if c == '\\' => {
                chars.next();
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '-' && chars.peek() == Some(&'-') => return true,
            None => {}
        }
    }
    quote.is_some()
}

/// The name expression ending at the end of `text`, e.g. "Strip[0]" from
/// "if Strip[0]", or "Bus[1].mode". `None` if it doesn't end in one.
fn expression_before(text: &str) -> Option<String> {
    let len: usize = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'))
        .map(char::len_utf8)
        .sum();
    let expression = text[text.len() - len..].trim_start_matches('.');
    (expression.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')).then(|| expression.to_string())
}

/// Follow `name = Strip[0]` style aliases (the latest before `at`) one step, so
/// `mic.` after `local mic = Strip[0]` completes like `Strip[0].`.
fn resolve(path: &str, text: &str, at: usize) -> String {
    let head_len = path.find(['.', '[']).unwrap_or(path.len());
    let head = &path[..head_len];
    if GLOBALS.iter().any(|(name, ..)| *name == head) {
        return path.to_string();
    }
    let before = &text[..at.min(text.len())];
    let assigned = before.lines().rev().find_map(|line| {
        let line = line.trim().strip_prefix("local ").unwrap_or(line.trim());
        let (name, value) = line.split_once('=')?;
        if name.trim() != head || value.starts_with('=') {
            return None;
        }
        let value = value.trim().trim_end_matches(';').trim();
        (expression_before(value)? == value).then(|| value.to_string())
    });
    match assigned {
        Some(target) => format!("{target}{}", &path[head_len..]),
        None => path.to_string(),
    }
}

fn item(label: &str, insert: impl Into<String>, detail: &str, kind: Kind) -> Item {
    Item { label: label.to_string(), insert: insert.into(), detail: detail.to_string(), kind }
}

fn params(list: &[(&str, &str)]) -> Vec<Item> {
    list.iter().map(|(name, detail)| item(name, *name, detail, Kind::Parameter)).collect()
}

fn globals(text: &str, cursor: usize) -> Vec<Item> {
    let mut items: Vec<Item> = GLOBALS.iter().map(|(name, insert, detail, kind)| item(name, *insert, detail, *kind)).collect();
    items.extend(KEYWORDS.iter().map(|k| item(k, *k, "keyword", Kind::Keyword)));
    // The script's own names: its variables and functions.
    let mut seen: Vec<&str> = Vec::new();
    let mut words = text.char_indices().peekable();
    while let Some((i, c)) = words.next() {
        if !(c.is_ascii_alphabetic() || c == '_') || text[..i].ends_with(|p: char| p.is_ascii_alphanumeric() || p == '_' || p == '.') {
            continue;
        }
        let end = text[i..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(text.len(), |n| i + n);
        while words.peek().is_some_and(|(j, _)| *j < end) {
            words.next();
        }
        let word = &text[i..end];
        // Not the word being typed, and only what isn't already listed.
        if (i..=end).contains(&cursor) || word.len() < 2 || seen.contains(&word) || KEYWORDS.contains(&word) || GLOBALS.iter().any(|(n, ..)| *n == word) {
            continue;
        }
        seen.push(word);
        items.push(item(word, word, "defined in this script", Kind::Variable));
    }
    items
}

/// What can follow `path.`.
fn members(path: &str, names: &Names) -> Vec<Item> {
    let segments = path_segments(path);
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
    let bus_buttons = |what: &str| -> Vec<Item> {
        names.buses.iter().map(|b| Item { label: b.clone(), insert: b.clone(), detail: format!("{what} {b}, 0 or 1"), kind: Kind::Parameter }).collect()
    };
    match segs.as_slice() {
        ["strip", "#"] => {
            let mut items = params(STRIP);
            items.extend(bus_buttons("send to"));
            items
        }
        ["bus", "#"] => {
            let mut items = params(BUS);
            items.push(item("EQ", "EQ.", "the bus EQ: EQ.on", Kind::Group));
            items.push(item("mode", "mode.", "bus mode switches: mode.TVMix = 1...", Kind::Group));
            items
        }
        ["bus", "#", "eq"] => vec![item("on", "on", "EQ switch, 0 or 1", Kind::Parameter)],
        ["bus", "#", "mode"] => BUS_MODES.iter().map(|(name, detail)| item(name, *name, &format!("= 1: {detail}"), Kind::Parameter)).collect(),
        ["command"] => {
            let mut items = params(COMMAND);
            items.push(item("Button", "Button[", "macro buttons: Command.Button[i].State = 1 runs macro i", Kind::Group));
            items
        }
        ["command", "button", "#"] => vec![item("State", "State", "= 1: run this macro button", Kind::Parameter)],
        ["recorder"] => {
            let mut items = params(RECORDER);
            items.push(item("mode", "mode.", "recorder modes", Kind::Group));
            items.push(item("ArmStrip", "ArmStrip[", "arm input i for recording, 0 or 1", Kind::Group));
            items.push(item("ArmBus", "ArmBus[", "arm bus i for recording, 0 or 1", Kind::Group));
            items.extend(bus_buttons("play into"));
            items
        }
        ["recorder", "mode"] => params(RECORDER_MODES),
        ["option"] => vec![item("delay", "delay[", "bus delay: Option.delay[i], 0 to 500 ms", Kind::Group)],
        [library] => LIBRARIES
            .iter()
            .find(|(name, _)| name == library)
            .map(|(_, functions)| {
                functions
                    .iter()
                    .map(|(name, detail)| {
                        let call = detail.contains('(');
                        item(name, if call { format!("{name}(") } else { name.to_string() }, detail, if call { Kind::Function } else { Kind::Parameter })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// What can go in `path[`: the strips, buses or buttons, by number.
fn indices(path: &str, names: &Names) -> Vec<Item> {
    let segments = path_segments(path);
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
    let numbered = |labels: &[String], then: &str| -> Vec<Item> {
        labels.iter().enumerate().map(|(i, label)| Item { label: i.to_string(), insert: format!("{i}]{then}"), detail: label.clone(), kind: Kind::Index }).collect()
    };
    match segs.as_slice() {
        ["strip"] => numbered(&names.strips, "."),
        ["bus"] => numbered(&names.buses, "."),
        ["command", "button"] => numbered(&names.buttons, "."),
        ["recorder", "armstrip"] => numbered(&names.strips, ""),
        ["recorder", "armbus"] | ["option", "delay"] => numbered(&names.buses, ""),
        _ => Vec::new(),
    }
}

/// "Strip[0].A1" → strip, #, a1: lower-case names, with indices as "#".
fn path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let (name, indexed) = match part.find('[') {
            Some(i) => (&part[..i], true),
            None => (part, false),
        };
        if !name.is_empty() {
            out.push(name.to_ascii_lowercase());
        }
        if indexed {
            out.push("#".into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Mixer;
    use crate::script::params::{self, Host, Lookup, RecorderState};

    fn names() -> Names {
        let mixer = Mixer::default();
        Names {
            strips: mixer.strips.iter().map(|s| s.label.clone()).collect(),
            buses: mixer.buses.iter().map(|b| b.key.clone()).collect(),
            buttons: vec!["Mic to B1".into()],
        }
    }

    fn labels(text: &str) -> Vec<String> {
        complete(text, text.len(), &names()).map(|c| c.items.into_iter().map(|i| i.label).collect()).unwrap_or_default()
    }

    #[test]
    fn members_follow_the_dot() {
        let strip = labels("Strip[0].");
        assert!(["Mute", "Gain", "FadeTo", "A1", "B2"].iter().all(|n| strip.contains(&n.to_string())), "{strip:?}");
        assert!(!strip.contains(&"A4".to_string()), "only this layout's buses");
        assert_eq!(labels("Strip[0].mu"), ["Mute"], "case doesn't matter");
        assert_eq!(labels("Bus[1].mode.TV"), ["TVMix"]);
        assert_eq!(labels("Recorder.mode."), ["recbus", "PlayOnLoad", "Loop", "MultiTrack"]);
        assert_eq!(labels("x = math.fl"), ["floor"]);
        assert_eq!(labels("Command.Button[0]."), ["State"]);
    }

    #[test]
    fn indices_show_what_they_are() {
        let c = complete("Strip[", 6, &names()).unwrap();
        assert_eq!((c.items[0].label.as_str(), c.items[0].insert.as_str(), c.items[0].detail.as_str()), ("0", "0].", "Hardware Input 1"));
        assert_eq!(c.items.len(), 5);
        assert_eq!(labels("Bus[").last().map(String::as_str), Some("4"));
        assert_eq!(complete("Recorder.ArmBus[", 16, &names()).unwrap().items[0].insert, "0]");
    }

    #[test]
    fn top_level_names_keywords_and_the_scripts_own() {
        assert_eq!(labels("St"), ["Strip", "string"], "case-insensitive, so the string library too");
        let text = "local volume = 3\nvo";
        assert_eq!(labels(text), ["volume"]);
        assert!(labels("lo").contains(&"local".to_string()));
        let c = complete("Re", 2, &names()).unwrap();
        let recorder = c.items.iter().find(|i| i.label == "Recorder").unwrap();
        assert_eq!(recorder.insert, "Recorder.", "groups lead straight into their members");
    }

    #[test]
    fn aliases_complete_like_what_they_name() {
        assert!(labels("local mic = Strip[0]\nmic.").contains(&"Gain".to_string()));
        assert!(labels("bus = Bus[2]\nbus.mo").contains(&"mode".to_string()));
    }

    #[test]
    fn nothing_in_comments_strings_or_numbers() {
        assert!(complete("-- Strip[0].", 12, &names()).is_none());
        assert!(complete("print(\"Strip[0].", 16, &names()).is_none());
        assert!(complete("x = 12", 6, &names()).is_none());
        assert!(complete("Strip[0].Mute", 13, &names()).is_none(), "nothing left to add");
    }

    #[test]
    fn the_parameter_under_the_cursor_is_described() {
        let text = "local mic = Strip[0]\nmic.Gain = -6";
        let cursor = text.find("Gain").unwrap() + 2;
        let (path, detail) = describe(text, cursor, &names()).unwrap();
        assert_eq!(path, "Strip[0].Gain");
        assert!(detail.contains("-60 to +12"));
        assert_eq!(describe("print(1)", 3, &names()).map(|(p, _)| p), Some("print".into()));
        assert!(describe("x = 1", 0, &names()).is_none());
    }

    /// Every listed parameter is one `params` knows, on both kinds of strip.
    #[test]
    fn listed_parameters_exist() {
        let mut host = Host::new(Mixer::default(), false, RecorderState::default(), 1);
        let n = names();
        let check = |host: &mut Host, object: &str| {
            for item in members(object, &n) {
                let name = format!("{object}.{}", item.insert.trim_end_matches(['.', '[']));
                let found = params::get(host, &name);
                if item.kind == Kind::Group && item.insert.ends_with('[') {
                    assert!(params::get(host, &format!("{name}[0]")).is_ok(), "{name}[0]");
                } else {
                    assert!(matches!(found, Ok(Lookup::Value(_)) | Ok(Lookup::Group)), "{name}: {found:?}");
                }
            }
        };
        for object in ["Strip[0]", "Strip[4]", "Bus[0]", "Bus[0].EQ", "Bus[0].mode", "Command", "Command.Button[0]", "Recorder", "Recorder.mode"] {
            check(&mut host, object);
        }
    }
}
