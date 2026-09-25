use tokio::sync::broadcast;

pub struct InProcBroker {
    tx: broadcast::Sender<String>,
}

impl InProcBroker {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    pub fn publish(&self, event: &str) {
        let _ = self.tx.send(event.to_string());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for InProcBroker {
    fn default() -> Self {
        Self::new()
    }
}