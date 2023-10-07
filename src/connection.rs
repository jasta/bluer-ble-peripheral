use crate::responder::BluerResponder;
use crate::writer::BluerWriter;
use ble_peripheral::bluetooth_address::BluetoothAddress;
use ble_peripheral::gatt_connection::GattConnection;
use bluer::Address;

#[derive(Debug, Clone)]
pub struct BluerConnection {
  address: BluetoothAddress,
}

impl BluerConnection {
  pub fn new(address: &Address) -> Self {
    Self {
      address: BluetoothAddress(address.0),
    }
  }
}

impl GattConnection for BluerConnection {
  type SystemError = bluer::Error;
  type Responder = BluerResponder;
  type Writer = BluerWriter;

  fn peer_address(&self) -> &BluetoothAddress {
    &self.address
  }
}
