//! System tray icon. On Linux it's an AppIndicator/StatusNotifier item, which
//! needs GTK running on a thread of its own; Linux trays only offer the menu
//! (clicks on the icon itself open it).

use std::sync::mpsc;

use eframe::egui;
#[cfg(any(windows, target_os = "linux"))]
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
#[cfg(any(windows, target_os = "linux"))]
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub enum TrayCommand {
    Show,
    Quit,
}

#[cfg(any(windows, target_os = "linux"))]
const SHOW_ID: &str = "show";
#[cfg(any(windows, target_os = "linux"))]
const QUIT_ID: &str = "quit";

pub struct Tray {
    #[cfg(windows)]
    _icon: TrayIcon,
    commands: mpsc::Receiver<TrayCommand>,
}

impl Tray {
    /// Create the icon. Events wake the UI (even while the window is hidden) and
    /// arrive through [`Tray::poll`].
    pub fn new(ctx: &egui::Context, icon_png: &'static [u8]) -> Result<Self, String> {
        #[cfg(windows)]
        let icon = build(icon_png)?;
        #[cfg(target_os = "linux")]
        linux::create(icon_png)?;
        #[cfg(any(windows, target_os = "linux"))]
        {
            Ok(Self {
                #[cfg(windows)]
                _icon: icon,
                commands: forward_events(ctx),
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (ctx, icon_png);
            Err("the tray icon isn't supported on this platform yet".into())
        }
    }

    pub fn poll(&self) -> Vec<TrayCommand> {
        self.commands.try_iter().collect()
    }
}

#[cfg(target_os = "linux")]
impl Drop for Tray {
    fn drop(&mut self) {
        linux::remove();
    }
}

/// Route menu picks and icon clicks to a channel, waking the UI for each.
#[cfg(any(windows, target_os = "linux"))]
fn forward_events(ctx: &egui::Context) -> mpsc::Receiver<TrayCommand> {
    let (tx, commands) = mpsc::channel();
    {
        let (tx, ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = match e.id.0.as_str() {
                QUIT_ID => TrayCommand::Quit,
                SHOW_ID => TrayCommand::Show,
                _ => return,
            };
            let _ = tx.send(cmd);
            ctx.request_repaint();
        }));
    }
    let ctx = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        let show = matches!(
            e,
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { .. }
        );
        if show {
            let _ = tx.send(TrayCommand::Show);
            ctx.request_repaint();
        }
    }));
    commands
}

/// Build the icon and its menu on the current thread (the GTK thread on Linux).
#[cfg(any(windows, target_os = "linux"))]
fn build(icon_png: &[u8]) -> Result<TrayIcon, String> {
    let show = MenuItem::with_id(MenuId::new(SHOW_ID), "Show OpenMeeter", true, None);
    let quit = MenuItem::with_id(MenuId::new(QUIT_ID), "Shut Down OpenMeeter", true, None);
    let menu = Menu::with_items(&[&show, &PredefinedMenuItem::separator(), &quit]).map_err(|e| e.to_string())?;

    let icon = image::load_from_memory(icon_png).map_err(|e| e.to_string())?;
    let small = icon.resize(32, 32, image::imageops::FilterType::Lanczos3).into_rgba8();
    let (w, h) = small.dimensions();
    let icon = tray_icon::Icon::from_rgba(small.into_raw(), w, h).map_err(|e| e.to_string())?;
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("OpenMeeter")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
mod linux {
    //! GTK may only ever run on one thread per process, so a single thread
    //! hosts the icon for the app's lifetime; the UI asks it to create or
    //! remove the icon by message.

    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::Duration;

    use gtk::glib;

    enum Message {
        Create { icon_png: &'static [u8], reply: mpsc::Sender<Result<(), String>> },
        Remove,
    }

    fn thread() -> &'static Mutex<mpsc::Sender<Message>> {
        static THREAD: OnceLock<Mutex<mpsc::Sender<Message>>> = OnceLock::new();
        THREAD.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new().name("tray".into()).spawn(move || run(rx)).expect("failed to spawn tray thread");
            Mutex::new(tx)
        })
    }

    fn run(rx: mpsc::Receiver<Message>) {
        if let Err(e) = gtk::init() {
            let e = format!("GTK unavailable: {e}");
            for message in rx {
                if let Message::Create { reply, .. } = message {
                    let _ = reply.send(Err(e.clone()));
                }
            }
            return;
        }
        let mut icon = None;
        glib::timeout_add_local(Duration::from_millis(50), move || {
            for message in rx.try_iter() {
                match message {
                    Message::Create { icon_png, reply } => {
                        let built = super::build(icon_png).map(|built| icon = Some(built));
                        let _ = reply.send(built);
                    }
                    Message::Remove => icon = None,
                }
            }
            glib::ControlFlow::Continue
        });
        gtk::main();
    }

    pub fn create(icon_png: &'static [u8]) -> Result<(), String> {
        let (reply, result) = mpsc::channel();
        let sent = thread().lock().map_err(|e| e.to_string())?.send(Message::Create { icon_png, reply });
        sent.map_err(|_| "the tray thread stopped".to_string())?;
        result.recv_timeout(Duration::from_secs(5)).map_err(|_| "the tray icon did not start".to_string())?
    }

    pub fn remove() {
        if let Ok(tx) = thread().lock() {
            let _ = tx.send(Message::Remove);
        }
    }
}
