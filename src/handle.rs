use tokio::sync::mpsc;
use crate::event::Event;

pub struct BluerHandle {
    pub tx: mpsc::UnboundedSender<Event>,
}

impl Drop for BluerHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Event::OnHandleDrop);
    }
}
