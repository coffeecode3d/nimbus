mod credentials;
mod engine;
mod installer;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let app = tauri::Builder::default()
    .manage(engine::EngineState::default())
    .invoke_handler(tauri::generate_handler![
      credentials::credentials_save,
      credentials::credentials_load,
      credentials::credentials_delete,
      engine::engine_info,
      installer::engine_install,
      engine::engine_start,
      engine::engine_authenticate,
      engine::engine_disarm,
      engine::engine_send,
      engine::engine_stop,
      engine::servers_latency
    ])
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      Ok(())
    })
    .build(tauri::generate_context!())
    .expect("error while building tauri application");

  app.run(|handle, event| {
    if let tauri::RunEvent::ExitRequested { .. } = event {
      let state = handle.state::<engine::EngineState>();
      engine::shutdown(&state);
    }
  });
}
