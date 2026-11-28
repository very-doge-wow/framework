//! The Framework desktop editor (egui/eframe with the wgpu renderer).

pub mod audio_out;
pub mod playback;
pub mod state;
pub mod theme;
pub mod ui;
pub mod worker;

pub use state::EditorState;
pub use ui::FrameworkApp;

pub fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Framework")
            .with_app_id("framework")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn logging_and_window_options() {
        super::init_logging();
        super::init_logging();
        let options = super::native_options();
        assert!(matches!(options.renderer, eframe::Renderer::Wgpu));
    }
}
