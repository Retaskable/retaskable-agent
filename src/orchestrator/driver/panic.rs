//! Reading what a caught panic said.

/// What a caught panic said.
pub(super) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    (panic.downcast_ref::<&str>().map(ToString::to_string))
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}
