use super::Client;
impl Client {
    pub(crate) fn test_resources(&self) -> Option<crate::testkit::ResourceSnapshot> {
        let inner = self.inner.as_ref()?;
        let state = inner.state.try_lock().ok()?;
        let (diagnostic_bytes, diagnostic_windows, pending_diagnostics) =
            state.diagnostics.resources();
        let (operation_slots, operation_metadata_bytes) = inner.operations.resources();
        let (process_client_slots, process_callback_workers) = crate::runtime::resources();
        Some(crate::testkit::ResourceSnapshot {
            buffered_events: state.owned,
            buffered_bytes: state.bytes,
            diagnostic_bytes,
            diagnostic_windows,
            pending_diagnostics,
            operation_slots,
            operation_metadata_bytes,
            process_client_slots,
            process_callback_workers,
        })
    }
}
