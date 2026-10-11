//! Settings → "Kayıtları aç": the core's log folder (`<app data>/logs`, where `dexd --local` and
//! the in-app core both write) in the file manager.

#[tauri::command]
pub fn logs_open(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = crate::app_paths::get(&app).data_dir.join("logs");
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}
