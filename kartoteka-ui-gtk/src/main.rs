//! `kartoteka-gtk` — the Linux GTK4/libadwaita frontend. A thin shell over the headless
//! `fond-*` crates; all library logic lives there (`docs/ARCHITECTURE.md` §4).

mod config;
mod github;
mod secret_store;
mod ui;
mod webdav;
mod webmeta;

use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;

use config::Config;

const APP_ID: &str = "io.github.calstfrancis.Kartoteka";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_open(|app, files, _| {
        let window = app
            .windows()
            .into_iter()
            .next()
            .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
            .unwrap_or_else(|| open_window(app));
        window.present();
        for path in files.iter().filter_map(|f| f.path()) {
            let _ = gtk4::prelude::WidgetExt::activate_action(
                &window,
                "win.add-path",
                Some(&path.to_string_lossy().to_string().to_variant()),
            );
        }
    });

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
        open_window(app).present();
    });

    app.run()
}

fn open_window(app: &adw::Application) -> adw::ApplicationWindow {
    ui::styles::load_global_css();
    ui::app_window::build(app, Config::load())
}
