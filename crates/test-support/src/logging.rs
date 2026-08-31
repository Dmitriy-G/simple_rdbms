use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct CaptureBuf(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn captured_events(buf: &CaptureBuf) -> Vec<serde_json::Value> {
    let bytes = buf.0.lock().unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("captured line is valid JSON"))
        .collect()
}

pub fn set_capturing_subscriber(capture: &CaptureBuf) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_writer({
            let capture = capture.clone();
            move || capture.clone()
        })
        .finish();
    tracing::subscriber::set_default(subscriber)
}
