mod bookmarks;
mod commands;
#[cfg(test)]
mod fake_backend;
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(state::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::connect_sftp,
            commands::connect_s3,
            commands::connect_azblob,
            commands::share_link,
            commands::disconnect,
            commands::list_dir,
            commands::download_file,
            commands::upload_file,
            commands::delete_entry,
            commands::rename_entry,
            commands::make_dir,
            commands::bookmarks_list,
            commands::bookmark_save,
            commands::bookmark_delete,
            commands::connect_bookmark,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
