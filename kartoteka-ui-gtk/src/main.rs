//! `kartoteka-gtk` — the Linux GTK4/libadwaita frontend. A thin shell over the headless
//! `fond-*` crates; all library logic lives there (`docs/ARCHITECTURE.md` §4).

mod config;
mod github;
mod secret_store;
mod ui;
mod webdav;
mod webmeta;

use gtk4::prelude::*;
use libadwaita as adw;

use config::Config;

const APP_ID: &str = "io.github.calstfrancis.Kartoteka";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(|app| {
        // GApplication's default single-instance behavior routes a second launch (e.g. a
        // flatpak re-run, or D-Bus activation from another app like Zerkalo's "K" button)
        // through `activate` on the *existing* process rather than starting a new one — but
        // without this check it built a brand-new window every time instead of raising the
        // one already open.
        if let Some(window) = app.windows().first() {
            window.present();
            return;
        }
        ui::styles::load_global_css();
        let config = Config::load();
        let window = ui::app_window::build(app, config);
        window.present();
    });

    app.run()
}
