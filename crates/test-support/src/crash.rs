use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use storage::block_device::DurabilityModel;

use crate::devices::{DeviceTriple, faulty_devices};

pub trait CrashWorkload {
    type Handle;
    type State: PartialEq + std::fmt::Debug;

    fn item_count(&self) -> usize;

    fn open(
        &self,
        dir: &Path,
        devices: DeviceTriple,
    ) -> Result<Self::Handle, Box<dyn std::error::Error>>;

    fn drive(&self, handle: &mut Self::Handle) -> usize;

    fn expected_state(&self, safe_prefix: usize)
    -> Result<Self::State, Box<dyn std::error::Error>>;

    fn observed_state(
        &self,
        safe_prefix: usize,
        handle: &mut Self::Handle,
    ) -> Result<Self::State, Box<dyn std::error::Error>>;
}

pub fn assert_workload_is_crash_safe<W: CrashWorkload>(
    workload: &W,
    model: DurabilityModel,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_writes = {
        let dir = tempfile::tempdir()?;
        let counter = Arc::new(AtomicU64::new(0));
        let devices = faulty_devices(dir.path(), &counter, u64::MAX, model)?;
        let mut handle = workload.open(dir.path(), devices)?;
        workload.drive(&mut handle);
        counter.load(Ordering::Relaxed)
    };
    assert!(total_writes > 0, "workload must perform at least one write");

    for fail_at in 1..=total_writes {
        let dir = tempfile::tempdir()?;
        let dir_path = dir.path();

        let safe_prefix = {
            let counter = Arc::new(AtomicU64::new(0));
            let devices = faulty_devices(dir_path, &counter, fail_at, model)?;
            match workload.open(dir_path, devices) {
                Ok(mut handle) => workload.drive(&mut handle),
                Err(_) => 0,
            }
        };

        for recovery_fail_at in [1u64, 2] {
            let counter = Arc::new(AtomicU64::new(0));
            if let Ok(devices) = faulty_devices(dir_path, &counter, recovery_fail_at, model) {
                let _ = workload.open(dir_path, devices);
            }
        }

        let counter = Arc::new(AtomicU64::new(0));
        let devices = faulty_devices(dir_path, &counter, u64::MAX, model)?;
        let mut recovered = workload.open(dir_path, devices)?;

        let observed = workload.observed_state(safe_prefix, &mut recovered).map_err(|err| {
            format!(
                "model={model:?}, fail_at={fail_at}, safe_prefix={safe_prefix}/{}: {err}",
                workload.item_count()
            )
        })?;
        let expected = workload.expected_state(safe_prefix)?;
        assert_eq!(
            observed,
            expected,
            "model={model:?}, fail_at={fail_at}, safe_prefix={safe_prefix}/{}: recovered state \
             must match replaying exactly the safely committed prefix",
            workload.item_count()
        );
    }
    Ok(())
}
