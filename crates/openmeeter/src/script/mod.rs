//! Lua macro scripts (Lua 5.3). Scripts live as `.lua` files in a `scripts`
//! folder beside the settings, and see the mixer through Voicemeeter's parameter
//! names (see `params`): `Strip[0].Mute = 1`, `Bus[0].FadeTo = "(-10, 500)"`,
//! `Recorder.play = 1`, plus the Remote API's `GetParameter`, `SetParameter` and
//! `SetParameters`.
//!
//! Scripts run on the UI thread, in a sandbox: no files, processes or OS access
//! (except `require` of other scripts in the folder), at most `TIME_LIMIT` per run
//! and `MEMORY_LIMIT` in all. Globals persist between runs, so a script can keep
//! state from one press to the next.

pub mod complete;
pub mod params;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::chunk::ChunkMode;
use mlua::{HookTriggers, Lua, LuaOptions, MetaMethod, StdLib, UserData, UserDataMethods, Value as LuaValue, Variadic, VmState};

use params::{Host, Input, Lookup, Op, Value};

pub const TIME_LIMIT: Duration = Duration::from_millis(250);
const MEMORY_LIMIT: usize = 32 << 20;
/// Lines of output the Scripts window keeps.
const OUTPUT_LINES: usize = 500;
/// The tables scripts start from.
const ROOTS: [&str; 5] = ["Strip", "Bus", "Command", "Recorder", "Option"];

pub struct OutputLine {
    pub text: String,
    pub error: bool,
}

pub struct Engine {
    lua: Lua,
    folder: PathBuf,
    deadline: Rc<Cell<Option<Instant>>>,
    printed: Rc<RefCell<Vec<String>>>,
    /// What scripts printed and how they failed, newest last.
    pub output: VecDeque<OutputLine>,
}

impl Engine {
    pub fn new(folder: PathBuf) -> Result<Self, String> {
        Self::build(folder).map_err(|e| format!("Lua failed to start: {e}"))
    }

    fn build(folder: PathBuf) -> mlua::Result<Self> {
        // No io, os, package or debug: scripts reach the outside world only through
        // the functions below. No coroutines either, which the time limit can't see into.
        let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8, LuaOptions::default())?;
        lua.set_memory_limit(MEMORY_LIMIT)?;
        let globals = lua.globals();
        for name in ["dofile", "loadfile", "load", "loadstring"] {
            globals.raw_set(name, LuaValue::Nil)?;
        }

        let deadline: Rc<Cell<Option<Instant>>> = Rc::default();
        let watch = deadline.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(1000), move |_, _| match watch.get() {
            Some(deadline) if Instant::now() > deadline => {
                Err(mlua::Error::runtime(format!("stopped: still running after {} ms", TIME_LIMIT.as_millis())))
            }
            _ => Ok(VmState::Continue),
        })?;

        let printed: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = printed.clone();
        let print = lua.create_function(move |_, args: Variadic<LuaValue>| {
            let words: mlua::Result<Vec<String>> = args.iter().map(LuaValue::to_string).collect();
            sink.borrow_mut().push(words?.join("\t"));
            Ok(())
        })?;
        globals.set("print", print)?;

        globals.set(
            "GetParameter",
            lua.create_function(|lua, name: String| match params::get(&mut *host(lua)?, &name).map_err(mlua::Error::runtime)? {
                Lookup::Value(value) => to_lua(lua, value),
                Lookup::Group => Err(mlua::Error::runtime(format!("{name} is a group of parameters, not one"))),
            })?,
        )?;
        globals.set(
            "SetParameter",
            lua.create_function(|lua, (name, value): (String, LuaValue)| {
                params::set(&mut *host(lua)?, &name, to_input(value)?, Op::Set).map_err(mlua::Error::runtime)
            })?,
        )?;
        globals.set(
            "SetParameters",
            lua.create_function(|lua, script: String| params::run(&mut *host(lua)?, &script).map_err(mlua::Error::runtime))?,
        )?;

        let os = lua.create_table()?;
        os.set("time", lua.create_function(|_, ()| Ok(chrono::Local::now().timestamp()))?)?;
        let start = Instant::now();
        os.set("clock", lua.create_function(move |_, ()| Ok(start.elapsed().as_secs_f64()))?)?;
        os.set(
            "date",
            lua.create_function(|_, format: Option<String>| {
                let format = format.unwrap_or_else(|| "%c".into());
                if chrono::format::StrftimeItems::new(&format).any(|item| matches!(item, chrono::format::Item::Error)) {
                    return Err(mlua::Error::runtime(format!("bad date format \"{format}\"")));
                }
                Ok(chrono::Local::now().format(&format).to_string())
            })?,
        )?;
        globals.set("os", os)?;

        lua.set_named_registry_value("openmeeter.loaded", lua.create_table()?)?;
        let library = folder.clone();
        globals.set(
            "require",
            lua.create_function(move |lua, name: String| {
                if !valid_name(&name) {
                    return Err(mlua::Error::runtime(format!("require \"{name}\": not a script name")));
                }
                let loaded: mlua::Table = lua.named_registry_value("openmeeter.loaded")?;
                if let Some(value) = loaded.get::<Option<LuaValue>>(name.as_str())? {
                    return Ok(value);
                }
                let path = library.join(format!("{name}.lua"));
                let code = std::fs::read_to_string(&path).map_err(|e| mlua::Error::runtime(format!("require \"{name}\": {e}")))?;
                let value: LuaValue = lua.load(code).set_name(format!("@{name}.lua")).set_mode(ChunkMode::Text).call(())?;
                let value = if value.is_nil() { LuaValue::Boolean(true) } else { value };
                loaded.set(name, value.clone())?;
                Ok(value)
            })?,
        )?;

        Ok(Self { lua, folder, deadline, printed, output: VecDeque::new() })
    }

    /// Run the script `name` from the scripts folder.
    pub fn run_file(&mut self, name: &str, host: Host) -> (Host, Result<(), String>) {
        let path = self.folder.join(format!("{name}.lua"));
        match std::fs::read_to_string(&path) {
            Ok(code) => self.run(name, &code, host),
            Err(e) => {
                let error = format!("{}: {e}", path.display());
                self.log(format!("{name}: {error}"), true);
                (host, Err(error))
            }
        }
    }

    /// Run `code` against `host`, returning it with the script's changes and requests.
    pub fn run(&mut self, name: &str, code: &str, host: Host) -> (Host, Result<(), String>) {
        self.lua.set_app_data(host);
        let result = self.exec(name, code);
        let host = self.lua.remove_app_data::<Host>().expect("set above");
        for line in self.printed.take() {
            log::info!("script {name}: {line}");
            self.log(line, false);
        }
        let result = result.map_err(|e| describe(&e));
        if let Err(e) = &result {
            log::warn!("script {name}: {e}");
            self.log(format!("{name}: {e}"), true);
        }
        (host, result)
    }

    fn exec(&self, name: &str, code: &str) -> mlua::Result<()> {
        let globals = self.lua.globals();
        // Put the roots back in case a script reassigned them, and reload libraries
        // so edits to them take effect.
        for root in ROOTS {
            globals.set(root, Node(root.to_string()))?;
        }
        self.lua.set_named_registry_value("openmeeter.loaded", self.lua.create_table()?)?;
        self.deadline.set(Some(Instant::now() + TIME_LIMIT));
        let result = self.lua.load(code).set_name(format!("@{name}.lua")).set_mode(ChunkMode::Text).exec();
        self.deadline.set(None);
        result
    }

    pub fn log(&mut self, text: String, error: bool) {
        self.output.push_back(OutputLine { text, error });
        while self.output.len() > OUTPUT_LINES {
            self.output.pop_front();
        }
    }
}

pub fn list(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.path().file_name()?.to_str()?.strip_suffix(".lua").map(str::to_string))
        .filter(|name| valid_name(name))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Letters, digits, spaces, `-` and `_`: safe as a file name anywhere.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
}

fn host(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, Host>> {
    lua.app_data_mut::<Host>().ok_or_else(|| mlua::Error::runtime("the mixer is only reachable while a macro runs"))
}

/// A parameter group, e.g. `Strip` or `Strip[0]`: indexing it reads a parameter or
/// gives a smaller group, assigning to it sets a parameter.
struct Node(String);

impl Node {
    fn child(&self, key: &LuaValue) -> mlua::Result<String> {
        match key {
            LuaValue::Integer(i) if *i >= 0 => Ok(format!("{}[{i}]", self.0)),
            LuaValue::Number(n) if *n >= 0.0 && n.fract() == 0.0 => Ok(format!("{}[{n}]", self.0)),
            LuaValue::String(s) => Ok(format!("{}.{}", self.0, s.to_str()?)),
            other => Err(mlua::Error::runtime(format!("{}[{}] isn't a parameter", self.0, other.to_string()?))),
        }
    }
}

impl UserData for Node {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, node, key: LuaValue| {
            let name = node.child(&key)?;
            let found = params::get(&mut *host(lua)?, &name).map_err(mlua::Error::runtime)?;
            match found {
                Lookup::Value(value) => to_lua(lua, value),
                Lookup::Group => lua.create_userdata(Node(name)).map(LuaValue::UserData),
            }
        });
        methods.add_meta_method(MetaMethod::NewIndex, |lua, node, (key, value): (LuaValue, LuaValue)| {
            let name = node.child(&key)?;
            params::set(&mut *host(lua)?, &name, to_input(value)?, Op::Set).map_err(mlua::Error::runtime)
        });
        methods.add_meta_method(MetaMethod::ToString, |_, node, ()| Ok(node.0.clone()));
    }
}

fn to_lua(lua: &Lua, value: Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Int(i) => LuaValue::Integer(i),
        Value::Num(n) => LuaValue::Number(n),
        Value::Str(s) => LuaValue::String(lua.create_string(s)?),
    })
}

fn to_input(value: LuaValue) -> mlua::Result<Input> {
    Ok(match value {
        LuaValue::Integer(i) => Input::Num(i as f64),
        LuaValue::Number(n) => Input::Num(n),
        LuaValue::Boolean(b) => Input::Num(if b { 1.0 } else { 0.0 }),
        LuaValue::String(s) => Input::Str(s.to_str()?.to_string()),
        LuaValue::Table(t) => Input::Pair(t.get(1)?, t.get(2)?),
        other => return Err(mlua::Error::runtime(format!("can't set a parameter to {}", other.type_name()))),
    })
}

/// The error as one line, with the script line it happened on.
fn describe(error: &mlua::Error) -> String {
    match error {
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(message) => message.clone(),
        mlua::Error::CallbackError { traceback, cause } => {
            let cause = describe(cause);
            // The first frame in a script file says where it went wrong.
            let place = traceback.lines().map(str::trim).find_map(|line| {
                let (file, rest) = line.split_once(".lua:")?;
                let line_no: String = rest.chars().take_while(char::is_ascii_digit).collect();
                (!line_no.is_empty()).then(|| format!("{file}.lua:{line_no}: "))
            });
            if cause.contains(".lua:") { cause } else { format!("{}{cause}", place.unwrap_or_default()) }
        }
        mlua::Error::MemoryError(_) => format!("stopped: used more than {} MB", MEMORY_LIMIT >> 20),
        other => other.to_string(),
    }
}

/// A new script's contents.
pub fn template(name: &str) -> String {
    format!(
        "-- {name}: an OpenMeeter macro script (Lua 5.3). Bind it to a hotkey in Menu > Macro Buttons.\n\
         -- Strips and buses count from 0 as in Voicemeeter: Strip[0] is the first input, Bus[0] is A1.\n\
         -- Switches read 0 or 1 (and 0 is true in Lua, so compare: if Strip[0].Mute == 1 then ...).\n\
         \n\
         Strip[0].Mute = 1 - Strip[0].Mute   -- toggle the first input's mute\n\
         print(\"mute is now\", Strip[0].Mute)\n"
    )
}

/// The API, for the Scripts window and the README.
pub const REFERENCE: &str = r#"Strips and buses count from 0, as in Voicemeeter: hardware inputs, then virtual
inputs; A1, A2..., then B1, B2...  Switches are 0 or 1 (true/false also work).

Strip[i].Mute  .Solo  .Mono  .MC          switches
Strip[i].A1 ... .B3                       output buttons (the buses this layout has)
Strip[i].Gain                             fader, -60 to +12 dB
Strip[i].FadeTo = "(dB, ms)"              fade the fader to dB over ms
Strip[i].FadeBy = {dB, ms}                fade by dB over ms (a {dB, ms} table works too)
Strip[i].Label                            name (text)
Strip[i].Comp  .Gate                      0 to 10 (hardware inputs)
Strip[i].Pan_x -0.5..0.5  .Pan_y          pan pad / surround panner
Strip[i].EQGain1 .EQGain2 .EQGain3        bass, mid, treble, -12 to +12 (virtual inputs)
Strip[i].Karaoke                          0 (off) to 4

Bus[i].Mute                               switch
Bus[i].Mono                               0 off, 1 mono, 2 stereo reverse
Bus[i].Gain  .FadeTo  .FadeBy             as for strips
Bus[i].EQ.on                              switch
Bus[i].mode.normal .Amix .Bmix .Repeat .Composite .TVMix .UpMix21 .UpMix41
          .UpMix61 .CenterOnly .LFEOnly .RearOnly
Bus[i].Label                              "A1", "B2"... (read only)
Option.delay[i]                           bus delay, 0 to 500 ms

Recorder.play  .pause  .stop  .record     1 = press; reading tells whether it's on
Recorder.replay  .ff  .rew                1 = press (play from the start / skip 10 s)
Recorder.load = "C:/clips/horn.wav"       load a file (plays if Play On Load is set)
Recorder.goto = "00:01:30"                or seconds; Recorder.position reads it
Recorder.A1 ... .B3                       playback buses
Recorder.gain                             playback level, dB
Recorder.ArmStrip[i]  .ArmBus[i]          arming
Recorder.mode.recbus .PlayOnLoad .Loop .MultiTrack
Recorder.channel                          1 or 2

Command.Restart = 1                       restart the audio engine
Command.Show = 1                          show the window (0 hides it)
Command.Shutdown  .Eject  .Reset          1 = do it (Reset clears the mixer)
Command.Load = "C:/path/settings.json"    load settings; Command.Save saves them
Command.Lock                              Lock GUI switch
Command.Button[i].State = 1               run macro button i (counting from 0)

GetParameter("Strip[0].Gain")             the same parameters by name
SetParameter("Strip[0].Gain", -6)
SetParameters("Strip[0].Gain += 3; Strip[0].A1 += 1")   Voicemeeter script syntax,
                                          including += and -= (+= 1 flips a switch)
print(...)                                to the Scripts window and the log
require("name")                           run name.lua from the scripts folder, once
                                          per run, and return what it returns
os.time()  os.clock()  os.date(fmt)

Globals keep their values between runs, so a script can count presses. A run is
stopped after 250 ms. Files, programs and the network are out of reach."#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Mixer;
    use params::{Command, RecorderState};

    fn folder() -> PathBuf {
        std::env::temp_dir().join(format!("openmeeter-scripts-{}", std::process::id()))
    }

    fn engine() -> Engine {
        Engine::new(folder()).unwrap()
    }

    fn host() -> Host {
        Host::new(Mixer::default(), false, RecorderState::default(), 1)
    }

    #[test]
    fn scripts_change_the_mixer_through_voicemeeter_names() {
        let mut e = engine();
        let code = r#"
            Strip[0].Mute = 1 - Strip[0].Mute
            Strip[1].B1 = true
            Bus[0].Gain = -6
            Strip[2].Label = "Game"
            SetParameters("Strip[3].Gain -= 3; Strip[0].A1 += 1")
            SetParameter("Recorder.B2", 1)
            Bus[1].FadeTo = {-10, 500}
            Recorder.play = 1
            print("gain", GetParameter("Bus[0].Gain"), Strip[0].Mute)
        "#;
        let (h, result) = e.run("test", code, host());
        assert_eq!(result, Ok(()));
        let m = &h.mixer;
        assert!(m.strips[0].mute && !m.strips[0].routes.contains("A1"));
        assert!(m.strips[1].routes.contains("B1"));
        assert_eq!((m.buses[0].gain_db, m.strips[3].gain_db), (-6.0, -3.0));
        assert_eq!(m.strips[2].label, "Game");
        assert!(m.recorder.playback_buses.contains("B2"));
        assert_eq!(
            h.commands,
            [Command::Fade { key: "A2".into(), from: 0.0, to: -10.0, ms: 500.0 }, Command::Recorder(params::Transport::Play)]
        );
        assert_eq!(e.output.back().map(|l| l.text.as_str()), Some("gain\t-6\t1"));
    }

    #[test]
    fn errors_name_the_line() {
        let mut e = engine();
        let (_, result) = e.run("bad", "local x = 1\nStrip[9].Mute = 1\n", host());
        let error = result.unwrap_err();
        assert!(error.starts_with("bad.lua:2:") && error.contains("Strip[0] to Strip[4]"), "{error}");
        let (_, result) = e.run("bad", "Strip[0].Mute = = 1", host());
        assert!(result.unwrap_err().starts_with("bad.lua:1:"));
    }

    #[test]
    fn runaway_and_escaping_scripts_are_stopped() {
        let mut e = engine();
        let started = Instant::now();
        let (_, result) = e.run("loop", "while true do end", host());
        assert!(result.unwrap_err().contains("stopped"));
        assert!(started.elapsed() < TIME_LIMIT * 4);
        for code in ["io.open('x')", "os.execute('x')", "dofile('x')", "load('return 1')", "require('../x')", "debug.getinfo(1)"] {
            assert!(e.run("escape", code, host()).1.is_err(), "{code} must fail");
        }
    }

    #[test]
    fn globals_persist_between_runs() {
        let mut e = engine();
        let code = "presses = (presses or 0) + 1\nStrip[0].Gain = -presses";
        let _ = e.run("count", code, host());
        let (h, _) = e.run("count", code, host());
        assert_eq!(h.mixer.strips[0].gain_db, -2.0);
    }

    #[test]
    fn scripts_load_from_the_folder_and_require_each_other() {
        let mut e = engine();
        std::fs::create_dir_all(folder()).unwrap();
        std::fs::write(folder().join("lib.lua"), "return { mute = function(i) Strip[i].Mute = 1 end }").unwrap();
        std::fs::write(folder().join("main.lua"), "require('lib').mute(1)").unwrap();
        let (h, result) = e.run_file("main", host());
        assert_eq!(result, Ok(()));
        assert!(h.mixer.strips[1].mute);
        assert!(list(&folder()).contains(&"main".to_string()));
        let _ = std::fs::remove_dir_all(folder());
    }
}
