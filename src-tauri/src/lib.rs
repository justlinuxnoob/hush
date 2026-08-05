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
pub mod ratelimit;
pub mod scan;
pub mod state;
pub mod store;
pub mod unsub;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // The only plugin. It opens the consent page and unsubscribe links in
        // the user's real browser; the web layer cannot call it directly.
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Before anything else, so a failure during setup is recorded too.
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = std::fs::create_dir_all(&dir);
                logging::init(&dir);
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
            commands::list_blocks,
            commands::preview_block_removal,
            commands::remove_block,
            commands::outcomes,
            commands::open_link,
            commands::set_mailto_mode,
            commands::data_location,
            commands::open_data_folder,
            commands::erase_everything,
        ])
        .run(tauri::generate_context!())
        .expect("Hush couldn't start");
}
