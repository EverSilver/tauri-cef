// The CEF helper binaries used by macOS app bundles are no longer built from a
// separate `cef-helper` crate. Each helper .app inside the final bundle embeds
// the main application binary itself — the `#[tauri::entry_point]` macro
// detects the Chromium subprocess via the `--type=` argv switch and routes
// execution into `tauri_runtime_cef::run_cef_helper_process`.
fn main() {}
