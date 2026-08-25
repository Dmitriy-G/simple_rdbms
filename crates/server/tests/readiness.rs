use server::health::{Readiness, ReadinessState};

#[test]
fn starts_not_ready() {
    let readiness = Readiness::new();
    assert_eq!(readiness.state(), ReadinessState::Starting);
}

#[test]
fn set_ready_reports_ready() {
    let readiness = Readiness::new();
    readiness.set_ready();
    assert_eq!(readiness.state(), ReadinessState::Ready);
}

#[test]
fn set_not_ready_reports_shutting_down_once_ready() {
    let readiness = Readiness::new();
    readiness.set_ready();
    readiness.set_not_ready();
    assert_eq!(readiness.state(), ReadinessState::ShuttingDown);
}

#[test]
fn state_names_are_distinct() {
    assert_eq!(ReadinessState::Starting.to_string(), "starting");
    assert_eq!(ReadinessState::Ready.to_string(), "ready");
    assert_eq!(ReadinessState::ShuttingDown.to_string(), "shutting_down");
}
