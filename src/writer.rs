use crate::err_util;
use ble_peripheral::gatt_connection::GattWriter;
use std::fmt::{Debug, Formatter};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct BluerWriter {
  pub tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl Debug for BluerWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BluerWriter").finish_non_exhaustive()
  }
}

impl GattWriter for BluerWriter {
  type SystemError = bluer::Error;

  fn write(&mut self, value: &[u8]) -> Result<(), Self::SystemError> {
    self
      .tx
      .send(value.to_owned())
      .map_err(|_| err_util::generic_err("unsubscribed"))
  }
}
