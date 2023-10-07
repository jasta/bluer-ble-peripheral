use crate::event::Event;
use tokio::sync::mpsc;

pub struct BluerHandle {
  pub tx: mpsc::UnboundedSender<Event>,
}

impl Drop for BluerHandle {
  fn drop(&mut self) {
    let _ = self.tx.send(Event::OnHandleDrop);
  }
}
