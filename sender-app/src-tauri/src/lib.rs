mod sender;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(sender::build_state())
        .invoke_handler(tauri::generate_handler![
            sender::get_pairing_info,
            sender::get_receivers,
            sender::get_mirror_status,
            sender::get_capabilities,
            sender::start_sharing,
            sender::set_audio_route,
            sender::export_diagnostics,
            sender::start_mirroring,
            sender::stop_mirroring,
            sender::get_firewall_help,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = sender::init_sender(handle).await {
                    eprintln!("init sender gagal: {e}");
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
