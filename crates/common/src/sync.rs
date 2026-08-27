use std::sync::LockResult;

pub fn recover_lock<T>(result: LockResult<T>, what: &'static str) -> T {
    result.unwrap_or_else(|poisoned| {
        tracing::error!(what, "lock was poisoned by a panicking holder; recovering its guard");
        poisoned.into_inner()
    })
}
