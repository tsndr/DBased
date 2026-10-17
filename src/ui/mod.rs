pub mod connection_page;
pub mod sidebar;
pub mod transfer_dialog;
pub mod window;
pub mod workspace;

#[cfg(test)]
pub(crate) mod test_util {
    use std::any::Any;
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::{OnceLock, mpsc};

    type GtkJob = Box<dyn FnOnce() + Send + 'static>;

    static GTK_TEST_THREAD: OnceLock<mpsc::Sender<GtkJob>> = OnceLock::new();

    pub(crate) fn run_gtk_test<F, R>(f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let tx = GTK_TEST_THREAD.get_or_init(|| {
            let (job_tx, job_rx) = mpsc::channel::<GtkJob>();
            std::thread::Builder::new()
                .name("gtk-test-thread".to_string())
                .spawn(move || {
                    let _ = libadwaita::init();
                    sourceview5::init();
                    crate::app::load_css();
                    while let Ok(job) = job_rx.recv() {
                        job();
                    }
                })
                .expect("spawn gtk-test-thread");
            job_tx
        });

        let (res_tx, res_rx) = mpsc::channel::<Result<R, Box<dyn Any + Send + 'static>>>();
        tx.send(Box::new(move || {
            let res = catch_unwind(AssertUnwindSafe(f));
            let _ = res_tx.send(res);
        }))
        .expect("send job to gtk-test-thread");

        match res_rx.recv().expect("receive result from gtk-test-thread") {
            Ok(val) => val,
            Err(panic_payload) => resume_unwind(panic_payload),
        }
    }
}
