use log::warn;
use ble_peripheral::att_error::AttError;
use ble_peripheral::gatt_connection::{GattResponder, Response};
use tokio::sync::oneshot;
use crate::err_util;

#[derive(Debug)]
pub struct BluerResponder {
    pub tx: Option<oneshot::Sender<Result<(u8, Vec<u8>), AttError>>>,
}

impl GattResponder for BluerResponder {
    type SystemError = bluer::Error;

    fn respond(
        &mut self,
        response: Result<Response<'_>, AttError>,
    ) -> Result<(), Self::SystemError> {
        let mapped = response.map(|r| (r.offset, r.value.to_owned()));
        self
            .tx
            .take()
            .ok_or_else(|| err_util::generic_err("Already sent response!"))?
            .send(mapped)
            .map_err(|_| err_util::generic_err("Server shutdown"))
    }
}

impl Drop for BluerResponder {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            warn!("Failed to call respond on GattResponder!");
            let _ = tx.send(Err(AttError::AttributeNotFound));
        }
    }
}
