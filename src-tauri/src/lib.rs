mod bookmarks;
mod commands;
mod edit;
#[cfg(test)]
mod fake_backend;
mod state;
mod transfers;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(state::AppState::default())
        .setup(|app| {
            let conns = app.state::<state::AppState>().connections.clone();
            let engine = transfers::init_engine(app.handle(), conns.clone());
            app.manage(engine);
            let edit = edit::init_edit(app.handle(), conns);
            app.manage(edit);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect_sftp,
            commands::connect_s3,
            commands::connect_azblob,
            commands::share_link,
            commands::disconnect,
            commands::list_dir,
            commands::enqueue_download,
            commands::enqueue_upload,
            commands::pause_transfer,
            commands::resume_transfer,
            commands::cancel_transfer,
            commands::list_transfers,
            commands::clear_completed,
            commands::delete_entry,
            commands::rename_entry,
            commands::make_dir,
            commands::bookmarks_list,
            commands::bookmark_save,
            commands::bookmark_delete,
            commands::connect_bookmark,
            commands::open_in_editor,
            commands::list_edit_sessions,
            commands::close_edit_session,
            commands::resolve_conflict,
            commands::preview_file,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // On quit, flush any pending edits still inside the debounce window so
            // a save the user believes succeeded is not lost (C1). Temp files are
            // preserved for conflicted/unflushed sessions; startup re-cleans.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let edit = app
                    .state::<std::sync::Arc<edit::EditRegistry>>()
                    .inner()
                    .clone();
                tauri::async_runtime::block_on(async move {
                    edit.flush_all().await;
                });
            }
        });
}
