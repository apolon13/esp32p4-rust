use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use slint::ComponentHandle;
use crate::control::ControlCmd;
use crate::AppWindow;

pub struct SecurityHandler {
    cmd_rx:  Receiver<ControlCmd>,
    app:     slint::Weak<AppWindow>,
    entered: Rc<RefCell<String>>,
}

impl SecurityHandler {
    pub fn new(app: &AppWindow, cmd_rx: Receiver<ControlCmd>) -> Self {
        let entered = Rc::new(RefCell::new(String::new()));
        register_pin_digit(app, &entered);
        register_pin_delete(app, &entered);
        register_pin_confirm(app, &entered);
        app.set_security_armed(true);
        Self { cmd_rx, app: app.as_weak(), entered }
    }

    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            apply_cmd(&app, cmd);
        }
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
        if ok { disarm(&app); } else { app.set_lock_pin_error(true); }
    });
}

// ── Command handling ──────────────────────────────────────────────────────────

fn apply_cmd(app: &AppWindow, cmd: ControlCmd) {
    match cmd {
        ControlCmd::Arm    => { log::info!("Security: arm");    app.set_security_armed(true);  }
        ControlCmd::Disarm => { log::info!("Security: disarm"); disarm(app); }
        ControlCmd::Silent => { log::info!("Security: silent"); app.set_security_alarm(false); }
        ControlCmd::Alarm  => { log::info!("Security: alarm");  app.set_security_alarm(true);  }
    }
}

fn disarm(app: &AppWindow) {
    app.set_security_armed(false);
    app.set_security_alarm(false);
    app.set_lock_pin_error(false);
}
