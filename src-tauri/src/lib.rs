//! Hush — quietly unsubscribe from bulk email.
//!
//! Everything here runs on the user's machine. There is no Hush server, no
//! telemetry, and no analytics. The only hosts this binary ever contacts are
//! Google's, and whatever unsubscribe endpoint the user explicitly chooses to
//! act on.
//!
//! The modules, roughly in the order the app uses them:
//!
//! * [`auth`] — OAuth with the user's own Google credentials.
//! * [`gmail`] — a metadata-only Gmail client.
//! * [`ratelimit`] — an adaptive limiter that keeps scans inside Gmail's quota.
//! * [`parse`] — `List-Unsubscribe` parsing. The safety gate.
//! * [`heuristics`] — flags senders whose mail looks transactional.
//! * [`store`] — local SQLite cache and preferences.
//! * [`unsub`] — carries out the unsubscribes the user picked.

pub mod auth;
pub mod commands;
pub mod error;
pub mod filters;
pub mod gmail;
pub mod heuristics;
pub mod logging;
pub mod model;
pub mod parse;
pub mod portable;
pub mod ratelimit;
pub mod scan;
pub mod state;
pub mod store;
pub mod unsub;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    // Desktop only, and not merely because the crate is: on Android the system
    // already guarantees one instance of an activity, so there is nothing to
    // guard against. `init` does not even exist off desktop, which is how the
    // first Android build failed.
    //
    // Two copies on one database would both scan, doubling an API quota that
    // already caps a large mailbox at forty minutes — and clicking a launcher
    // twice is not a request for a second app.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager as _;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        // Opens the consent page and unsubscribe links in the user's real
        // browser; the web layer cannot call it directly.
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Before anything else, so a failure during setup is recorded too.
            // Portable mode puts this beside the executable rather than in the
            // user's home, so running from a stick leaves the machine clean.
            if let Some(dir) = commands::data_dir(app.handle()) {
                let _ = std::fs::create_dir_all(&dir);
                logging::init(&dir);
                if crate::portable::data_dir().is_some() {
                    log::info!("portable mode: everything lives in {}", dir.display());
                }
            }
            let store = commands::open_store(app.handle())?;
            app.manage(state::AppState::new(store));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::status,
            commands::mark_welcome_seen,
            commands::save_credentials,
            commands::connect,
            commands::resume_session,
            commands::cancel_connect,
            commands::cancel_run,
            commands::disconnect,
            commands::start_scan,
            commands::cancel_scan,
            commands::list_senders,
            commands::set_never_touch,
            commands::sender_messages,
            commands::plan_unsubscribe,
            commands::run_unsubscribe,
            commands::diagnose,
            commands::list_blocks,
            commands::preview_block_removal,
            commands::remove_block,
            commands::outcomes,
            commands::open_link,
            commands::data_location,
            commands::open_data_folder,
            commands::erase_everything,
        ])
        .run(tauri::generate_context!())
        .expect("Hush couldn't start");
}
