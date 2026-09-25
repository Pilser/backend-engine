use crate::events::Event;
use std::sync::Mutex;

pub trait EventStream: Send + Sync + 'static {
    fn publish(&self, board: &str, event: &Event);
}

pub struct InMemoryEventStream {
    log: Mutex<Vec<Event>>,
}

impl InMemoryEventStream {
    pub fn new() -> Self {
        Self { log: Mutex::new(Vec::new()) }
    }

    pub fn last(&self) -> Option<Event> {
        self.log.lock().unwrap().last().cloned()
    }

    pub fn len(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

impl Default for InMemoryEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStream for InMemoryEventStream {
    fn publish(&self, _board: &str, event: &Event) {
        self.log.lock().unwrap().push(event.clone());
    }
}

pub type BoxStream = Box<dyn EventStream>;