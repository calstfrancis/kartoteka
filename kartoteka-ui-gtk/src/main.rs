//! `kartoteka-gtk` — the Linux GTK4/libadwaita frontend. A thin shell over the headless
//! `fond-*` crates; all library logic lives there (`docs/ARCHITECTURE.md` §4).

mod config;
mod github;
mod search_helper;
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
    // `--search-json` is a command-line helper, not the app: answer and leave before GTK starts.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--search-json") {
        search_helper::run(&args[1..]);
        return glib::ExitCode::SUCCESS;
    }

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
        // `kartoteka:KEY` (Porog's search results) selects that reference. GIO writes the
        // URI back as `kartoteka:///KEY`, so the slashes are dropped.
        for key in files.iter().filter_map(|f| {
            let uri = f.uri();
            let key = uri.strip_prefix("kartoteka:")?.trim_start_matches('/');
            (!key.is_empty()).then(|| key.to_string())
        }) {
            let _ = gtk4::prelude::WidgetExt::activate_action(
                &window,
                "win.select-key",
                Some(&key.to_variant()),
            );
        }
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

    // The Porog status line is written on a worker thread; let it finish.
    app.connect_shutdown(|_| fond_status::summary::flush());

    app.run()
}

fn open_window(app: &adw::Application) -> adw::ApplicationWindow {
    ui::styles::load_global_css();
    ui::app_window::build(app, Config::load())
}
