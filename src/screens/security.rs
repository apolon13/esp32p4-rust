use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use slint::ComponentHandle;
use crate::control::ControlCmd;
use crate::display::Display;
use crate::AppWindow;

const BACKLIGHT_DELAY: Duration = Duration::from_secs(60);

pub struct SecurityHandler {
    cmd_rx:        Receiver<ControlCmd>,
    app:           slint::Weak<AppWindow>,
    entered:       Rc<RefCell<String>>,
    arm_at:        Rc<Cell<Option<Instant>>>,
    backlight_off_at: Rc<Cell<Option<Instant>>>,
    backlight_on:  Rc<Cell<bool>>,
}

impl SecurityHandler {
    pub fn new(app: &AppWindow, cmd_rx: Receiver<ControlCmd>) -> Self {
        let entered          = Rc::new(RefCell::new(String::new()));
        let arm_at           = Rc::new(Cell::new(None::<Instant>));
        let backlight_off_at = Rc::new(Cell::new(None::<Instant>));
        let backlight_on     = Rc::new(Cell::new(true));
        register_pin_digit(app, &entered);
        register_pin_delete(app, &entered);
        register_pin_confirm(app, &entered, &arm_at);
        register_arm_now(app, &arm_at);
        app.set_security_armed(true);
        schedule_backlight_off(&backlight_off_at);
        Self { cmd_rx, app: app.as_weak(), entered, arm_at, backlight_off_at, backlight_on }
    }

    pub fn poll(&self, display: &Display, touched: bool) {
        let Some(app) = self.app.upgrade() else { return };
        if touched && !self.backlight_on.get() {
            display.backlight_on();
            self.backlight_on.set(true);
        }
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            apply_cmd(&app, cmd, &self.arm_at, &self.backlight_off_at, display, &self.backlight_on);
        }
        tick_timer(&app, &self.arm_at, &self.backlight_off_at);
        tick_backlight(display, &self.backlight_off_at, &self.backlight_on);
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

fn register_pin_confirm(app: &AppWindow, entered: &Rc<RefCell<String>>, arm_at: &Rc<Cell<Option<Instant>>>) {
    let app_weak = app.as_weak();
    let entered  = entered.clone();
    let arm_at   = arm_at.clone();
    app.on_lock_pin_confirm(move || {
        let app    = app_weak.upgrade().unwrap();
        let stored: String = app.get_settings_pin().into();
        let mut e  = entered.borrow_mut();
        let ok     = stored.is_empty() || *e == stored;
        e.clear();
        app.set_lock_pin_display("".into());
        if ok { disarm(&app, &arm_at); } else { app.set_lock_pin_error(true); }
    });
}

fn register_arm_now(app: &AppWindow, arm_at: &Rc<Cell<Option<Instant>>>) {
    let app_weak = app.as_weak();
    let arm_at   = arm_at.clone();
    app.on_arm_now(move || {
        let app = app_weak.upgrade().unwrap();
        do_arm(&app, &arm_at);
    });
}

// ── Command handling ──────────────────────────────────────────────────────────

fn apply_cmd(
    app: &AppWindow,
    cmd: ControlCmd,
    arm_at: &Rc<Cell<Option<Instant>>>,
    backlight_off_at: &Rc<Cell<Option<Instant>>>,
    display: &Display,
    backlight_on: &Rc<Cell<bool>>,
) {
    match cmd {
        ControlCmd::Arm    => {
            log::info!("Security: arm");
            do_arm(app, arm_at);
            schedule_backlight_off(backlight_off_at);
        }
        ControlCmd::Disarm => {
            log::info!("Security: disarm");
            disarm(app, arm_at);
            backlight_off_at.set(None);
            if !backlight_on.get() {
                display.backlight_on();
                backlight_on.set(true);
            }
        }
        ControlCmd::Silent => { log::info!("Security: silent"); app.set_security_alarm(false); }
        ControlCmd::Alarm  => { log::info!("Security: alarm");  app.set_security_alarm(true);  }
    }
}

fn do_arm(app: &AppWindow, arm_at: &Rc<Cell<Option<Instant>>>) {
    arm_at.set(None);
    app.set_arm_countdown_text("".into());
    app.set_security_armed(true);
}

fn disarm(app: &AppWindow, arm_at: &Rc<Cell<Option<Instant>>>) {
    let timeout_mins = app.get_settings_arm_timeout() as u64;
    let secs = if timeout_mins == 0 { 180 } else { timeout_mins * 60 };
    arm_at.set(Some(Instant::now() + Duration::from_secs(secs)));
    app.set_security_armed(false);
    app.set_security_alarm(false);
    app.set_lock_pin_error(false);
}

fn schedule_backlight_off(backlight_off_at: &Rc<Cell<Option<Instant>>>) {
    backlight_off_at.set(Some(Instant::now() + BACKLIGHT_DELAY));
}

// ── Auto-arm timer ────────────────────────────────────────────────────────────

fn tick_timer(app: &AppWindow, arm_at: &Rc<Cell<Option<Instant>>>, backlight_off_at: &Rc<Cell<Option<Instant>>>) {
    let Some(at) = arm_at.get() else { return };
    if Instant::now() >= at {
        arm_at.set(None);
        app.set_security_armed(true);
        schedule_backlight_off(backlight_off_at);
        log::info!("Security: auto-arm triggered");
    }
}

// ── Backlight timer ───────────────────────────────────────────────────────────

fn tick_backlight(display: &Display, backlight_off_at: &Rc<Cell<Option<Instant>>>, backlight_on: &Rc<Cell<bool>>) {
    let Some(at) = backlight_off_at.get() else { return };
    if Instant::now() >= at && backlight_on.get() {
        display.backlight_off();
        backlight_on.set(false);
        backlight_off_at.set(None);
        log::info!("Display: backlight off");
    }
}
