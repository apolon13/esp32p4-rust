use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use slint::ComponentHandle;
use crate::control::ControlCmd;
use crate::display::Display;
use crate::mqtt::EventPublisher;
use crate::AppWindow;

const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

pub struct SecurityHandler {
    cmd_rx:        Receiver<ControlCmd>,
    app:           slint::Weak<AppWindow>,
    last_activity: Rc<Cell<Instant>>,
    backlight_on:  Rc<Cell<bool>>,
    event_pub:     EventPublisher,
}

impl SecurityHandler {
    pub fn new(app: &AppWindow, cmd_rx: Receiver<ControlCmd>, event_pub: EventPublisher) -> Self {
        let entered       = Rc::new(RefCell::new(String::new()));
        let last_activity = Rc::new(Cell::new(Instant::now()));
        let backlight_on  = Rc::new(Cell::new(true));
        register_pin_digit(app, &entered);
        register_pin_delete(app, &entered);
        register_pin_confirm(app, &entered);
        register_arm_now(app, event_pub.clone());
        register_lock_screen(app);
        app.set_security_armed(true);
        Self { cmd_rx, app: app.as_weak(), last_activity, backlight_on, event_pub }
    }

    pub fn poll(&self, display: &Display, touched: bool) {
        let Some(app) = self.app.upgrade() else { return };
        if touched {
            self.last_activity.set(Instant::now());
            if !self.backlight_on.get() {
                display.backlight_on();
                self.backlight_on.set(true);
            }
        }
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            if !self.backlight_on.get() {
                display.backlight_on();
                self.backlight_on.set(true);
            }
            self.last_activity.set(Instant::now());
            apply_cmd(&app, cmd, &self.event_pub);
        }
        tick_inactivity(display, &app, &self.last_activity, &self.backlight_on);
    }
}

// ── PIN callbacks ─────────────────────────────────────────────────────────────

fn register_pin_digit(app: &AppWindow, entered: &Rc<RefCell<String>>) {
    let app_weak = app.as_weak();
    let entered  = entered.clone();
    app.on_lock_pin_digit(move |digit| {
        let app = app_weak.upgrade().unwrap();
        let mut e = entered.borrow_mut();
        if e.len() < 8 { e.push_str(digit.as_str()); }
        app.set_lock_pin_display("●".repeat(e.len()).into());
        app.set_lock_pin_error(false);
    });
}

fn register_pin_delete(app: &AppWindow, entered: &Rc<RefCell<String>>) {
    let app_weak = app.as_weak();
    let entered  = entered.clone();
    app.on_lock_pin_delete(move || {
        let app = app_weak.upgrade().unwrap();
        let mut e = entered.borrow_mut();
        e.pop();
        app.set_lock_pin_display("●".repeat(e.len()).into());
    });
}

fn register_pin_confirm(app: &AppWindow, entered: &Rc<RefCell<String>>) {
    let app_weak = app.as_weak();
    let entered  = entered.clone();
    app.on_lock_pin_confirm(move || {
        let app    = app_weak.upgrade().unwrap();
        let stored: String = app.get_settings_pin().into();
        let mut e  = entered.borrow_mut();
        let ok     = stored.is_empty() || *e == stored;
        e.clear();
        app.set_lock_pin_display("".into());
        if ok { unlock(&app); } else { app.set_lock_pin_error(true); }
    });
}

fn register_arm_now(app: &AppWindow, event_pub: EventPublisher) {
    let app_weak = app.as_weak();
    app.on_arm_now(move || {
        do_arm(&app_weak.upgrade().unwrap());
        event_pub.publish("armed");
    });
}

fn register_lock_screen(app: &AppWindow) {
    let app_weak = app.as_weak();
    app.on_lock_screen(move || { app_weak.upgrade().unwrap().set_ui_locked(true); });
}

// ── State transitions ─────────────────────────────────────────────────────────

fn do_arm(app: &AppWindow) {
    app.set_security_armed(true);
    app.set_ui_locked(false);
}

fn disarm(app: &AppWindow) {
    app.set_security_armed(false);
    app.set_security_alarm(false);
    app.set_ui_locked(false);
    app.set_lock_pin_error(false);
}

/// Разблокировать: снять и охрану (если стоит), и блокировку экрана.
fn unlock(app: &AppWindow) {
    if app.get_security_armed() || app.get_security_alarm() {
        disarm(app);
    } else {
        app.set_ui_locked(false);
        app.set_lock_pin_error(false);
    }
}

// ── Command handling ──────────────────────────────────────────────────────────

fn apply_cmd(app: &AppWindow, cmd: ControlCmd, event_pub: &EventPublisher) {
    match cmd {
        ControlCmd::Arm    => { log::info!("Security: arm");    do_arm(app);                   event_pub.publish("armed"); }
        ControlCmd::Disarm => { log::info!("Security: disarm"); disarm(app);                   event_pub.publish("disarmed"); }
        ControlCmd::Silent => { log::info!("Security: silent"); app.set_security_alarm(false); event_pub.publish("alarm_silenced"); }
        ControlCmd::Alarm  => { log::info!("Security: alarm");  app.set_security_alarm(true);  event_pub.publish("alarm"); }
    }
}

// ── Inactivity: backlight + screen lock ──────────────────────────────────────

fn tick_inactivity(
    display:       &Display,
    app:           &AppWindow,
    last_activity: &Rc<Cell<Instant>>,
    backlight_on:  &Rc<Cell<bool>>,
) {
    if backlight_on.get() && last_activity.get().elapsed() >= INACTIVITY_TIMEOUT {
        display.backlight_off();
        backlight_on.set(false);
        app.set_ui_locked(true);
        log::info!("Display: backlight off + screen locked (inactivity)");
    }
}
