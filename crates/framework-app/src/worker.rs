use framework_core::{ExportProfile, Project, Time};
use framework_media::{ExportProgress, PreviewRenderer, RgbaImage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Renders preview frames on a background thread. Only the most recent
/// request is processed, so scrubbing never builds up a backlog.
pub struct PreviewWorker {
    requests: Option<Sender<(Project, Time)>>,
    results: Receiver<Result<RgbaImage, String>>,
    handle: Option<JoinHandle<()>>,
}

impl PreviewWorker {
    pub fn spawn(width: u32, height: u32, wake: impl Fn() + Send + 'static) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<(Project, Time)>();
        let (res_tx, res_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut renderer = PreviewRenderer::new(width, height);
            while let Ok(mut req) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let result = renderer.render(&req.0, req.1).map_err(|e| e.to_string());
                if res_tx.send(result).is_err() {
                    break;
                }
                wake();
            }
        });
        Self {
            requests: Some(req_tx),
            results: res_rx,
            handle: Some(handle),
        }
    }

    pub fn request(&self, project: &Project, t: Time) {
        if let Some(tx) = &self.requests {
            let _ = tx.send((project.clone(), t));
        }
    }

    /// The newest finished frame, if any.
    pub fn poll(&self) -> Option<Result<RgbaImage, String>> {
        self.results.try_iter().last()
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportState {
    Running(ExportProgress),
    Done(PathBuf),
    Failed(String),
}

/// A project export running on a background thread.
pub struct ExportJob {
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ExportJob {
    pub fn start(
        project: Project,
        profile: ExportProfile,
        output: PathBuf,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(ExportState::Running(ExportProgress {
            frame: 0,
            total_frames: 0,
        })));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn) = (state.clone(), cancel.clone());
        let handle = std::thread::spawn(move || {
            let result = framework_media::export(&project, &profile, &output, &cn, |p| {
                *st.lock().expect("export state lock") = ExportState::Running(p);
                wake();
            });
            *st.lock().expect("export state lock") = match result {
                Ok(()) => ExportState::Done(output),
                Err(e) => ExportState::Failed(e.to_string()),
            };
            wake();
        });
        Self {
            state,
            cancel,
            handle: Some(handle),
        }
    }

    pub fn state(&self) -> ExportState {
        self.state.lock().expect("export state lock").clone()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state(), ExportState::Running(_))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn wait(mut self) -> ExportState {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framework_core::{MediaAsset, MediaKind};

    fn project_with_video(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("framework-app-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        framework_media::synthetic::write_test_video(&path, (64, 36), 10, 10, true).unwrap();
        let mut p = Project::new("t");
        let info = framework_media::probe(&path).unwrap();
        let id = p.add_media(MediaAsset::new(path, info));
        let track = p.default_track_for(MediaKind::Video);
        p.append_media(track, id).unwrap();
        p
    }

    #[test]
    fn preview_worker_renders_latest_request() {
        let p = project_with_video("prev.mp4");
        let worker = PreviewWorker::spawn(32, 18, || {});
        assert!(worker.poll().is_none());
        worker.request(&p, Time::from_seconds(0.5));
        let img = loop {
            if let Some(r) = worker.poll() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!((img.width, img.height), (32, 18));
    }

    #[test]
    fn export_job_completes_and_can_be_cancelled() {
        let p = project_with_video("exp.mp4");
        let mut profile = ExportProfile::builtin_by_id("draft-540p").unwrap();
        profile.video.width = 64;
        profile.video.height = 36;
        let out =
            std::env::temp_dir().join(format!("framework-app-job-{}.mp4", std::process::id()));
        let job = ExportJob::start(p.clone(), profile.clone(), out.clone(), || {});
        let _ = job.is_running();
        assert_eq!(job.wait(), ExportState::Done(out.clone()));

        let job = ExportJob::start(p, profile, out.clone(), || {});
        job.cancel();
        match job.wait() {
            ExportState::Failed(msg) => assert!(msg.contains("cancelled")),
            other => panic!("unexpected {other:?}"),
        }
        assert!(!out.exists());
    }

    #[test]
    fn export_job_reports_errors() {
        let job = ExportJob::start(
            Project::new("empty"),
            ExportProfile::default_profile(),
            PathBuf::from("unused.mp4"),
            || {},
        );
        assert!(matches!(job.wait(), ExportState::Failed(_)));
    }
}
