use std::sync::mpsc;

pub fn wait_for_shutdown_signal() -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    rx.recv()?;
    Ok(())
}
