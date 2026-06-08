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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
