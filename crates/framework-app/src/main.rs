#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    framework_app::init_logging();
    let project = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    eframe::run_native(
        "Framework",
        framework_app::native_options(),
        Box::new(move |cc| {
            let mut app = framework_app::FrameworkApp::new(&cc.egui_ctx);
            if let Some(path) = &project {
                app.open_project(path);
            }
            Ok(Box::new(app))
        }),
    )
}
