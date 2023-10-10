use ble_peripheral::prelude::*;
use bluer_ble_peripheral::BluerPeripheral;
use enumset::enum_set;
use log::{debug, error, info, trace, warn};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt::Debug;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::select;
use tokio::sync::oneshot;
use tokio::task::LocalSet;

const ECHO_SERVICE_UUID: UUID = UUID::Long(0xFEEDC0DE);
const ECHO_CHARACTERISTIC_UUID: UUID = UUID::Long(0xF00DC0DE00001);
const MY_MANUFACTURER_ID: u16 = 0xf00d;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), bluer::Error> {
  env_logger::init();

  let local_set = LocalSet::new();
  local_set.run_until(run_server()).await
}

async fn run_server() -> Result<(), bluer::Error> {
  let session = bluer::Session::new().await?;
  let adapter = session.default_adapter().await?;
  adapter.set_powered(true).await?;

  // Show what it's like to implement entirely against the traits...
  run_server_from_trait(BluerPeripheral::new(adapter)).await
}

async fn run_server_from_trait<P>(mut peripheral: P) -> Result<(), P::SystemError>
where
  P: Peripheral + Debug + 'static,
{
  peripheral.set_name("gatt_server")?;

  let service = GattService {
    uuid: ECHO_SERVICE_UUID,
    characteristics: &[GattCharacteristic {
      uuid: ECHO_CHARACTERISTIC_UUID,
      properties: enum_set!(GattCharacteristicProperty::Read | GattCharacteristicProperty::Write),
      permissions: enum_set!(
        GattCharacteristicPermission::Read | GattCharacteristicPermission::Write
      ),
      ..Default::default()
    }],
    ..Default::default()
  };

  let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
  let sm = EchoServer::new(shutdown_tx);
  let handle = peripheral.configure_gatt_server(&[service], sm)?;

  println!("Started echo server. Press Enter to exit.");
  let stdin = BufReader::new(tokio::io::stdin());
  let mut lines = stdin.lines();

  // TODO: Why doesn't this actually work to stop when we get an internal error!?!?
  select! {
    _ = lines.next_line() => {},
    _ = &mut shutdown_rx => println!("Internal shutdown, stopping!"),
  }

  // Important to move this to the end so we don't accidentally shut down the server!
  drop(handle);

  Ok(())
}

struct EchoServer<P: Peripheral> {
  shutdown_tx: Option<oneshot::Sender<()>>,
  echo_handle: Option<AttributeHandle>,
  advertiser: Option<P::Advertiser>,
  is_advertising: bool,
  last_value: Vec<u8>,
  writers: WritersManager<<P::Connection as GattConnection>::Writer>,
}

impl<P: Peripheral> EchoServer<P> {
  pub fn new(shutdown_tx: oneshot::Sender<()>) -> Self {
    Self {
      shutdown_tx: Some(shutdown_tx),
      echo_handle: None,
      advertiser: None,
      is_advertising: false,
      last_value: Vec::new(),
      writers: WritersManager::new(),
    }
  }

  fn start_advertising(&self) {
    let mut manufacturer_data = BTreeMap::new();
    manufacturer_data.insert(MY_MANUFACTURER_ID, vec![0x21, 0x22, 0x23, 0x24]);
    let adv = Advertisement {
      is_connectable: true,
      is_discoverable: true,
      manufacturer_data,
    };
    self.advertiser.clone().unwrap().request_start(adv);
  }

  fn set_last_value(&mut self, value: &[u8]) {
    self.last_value.clear();
    self.last_value.extend(value);

    if let Some(writers) = self.writers.lookup(&self.echo_handle.unwrap()) {
      for writer in writers {
        debug!("Updating active listener...");
        if let Err(e) = writer.write(&value) {
          warn!("Error updating listener: {e:?}");
        }
      }
    }
  }
}

impl<P: Peripheral + Debug> GattServerCallback<P> for EchoServer<P> {
  fn on_event(&mut self, event: GattServerEvent<'_, P>) {
    trace!("event: {event:?}");
    match event {
      GattServerEvent::ServerShutdown { error } => {
        error!("Server shutdown: {error:?}");
        let _ = self.shutdown_tx.take().map(|tx| tx.send(()));
      }
      GattServerEvent::ServerStarted {
        advertiser,
        handle_mapping,
      } => {
        info!("Server started!");

        for (uuid, handle) in handle_mapping {
          if uuid == &ECHO_CHARACTERISTIC_UUID {
            self.echo_handle = Some(*handle);
          }
        }

        self.advertiser = Some(advertiser);
        self.start_advertising();
      }
      GattServerEvent::AdvertisingStarted {
        remaining_connections,
      } => {
        info!("Advertising started");
        debug!("remaining_connections={remaining_connections:?}");
        self.is_advertising = true;
      }
      GattServerEvent::AdvertisingStopped { reason } => {
        info!("Advertising stopped: reason={reason:?}");
        self.is_advertising = false;
      }
      GattServerEvent::AdvertisingStartFail { reason } => {
        error!("Advertising start failed: {reason:?}");
        let _ = self.shutdown_tx.take().map(|tx| tx.send(()));
      }
      GattServerEvent::Connected { connection } => {
        info!("Accepted incoming connection: {connection:?}");
      }
      GattServerEvent::Disconnected { connection, reason } => {
        info!("Disconnection from {connection:?}: {reason:?}");
        if !self.is_advertising {
          self.start_advertising();
        }
      }
      GattServerEvent::MtuChanged { connection, mtu } => {
        info!("MTU changed on {connection:?}: mtu={mtu}");
      }
      GattServerEvent::ReadRequest {
        connection,
        handle,
        responder,
      } => {
        debug!("Got read request on {connection:?}: handle={handle}");
        if handle == self.echo_handle.unwrap() {
          if let Err(e) = responder.respond(Ok(Response::complete(&self.last_value))) {
            error!("Responder failed: {e:?}");
          }
        } else {
          warn!("Unknown handle={handle}");
        }
      }
      GattServerEvent::WriteRequest {
        connection,
        handle,
        responder,
        action,
        offset,
        value,
      } => {
        debug!("Got write request on {connection:?}: handle={handle}, action={action:?}");
        if handle == self.echo_handle.unwrap() {
          if responder.is_some() {
            error!("Shouldn't be a responder here!");
          }
          if offset > 0 {
            warn!("Ignoring write offset={offset}!");
          }
          self.set_last_value(value);
        } else {
          warn!("Unknown handle={handle}");
        }
      }
      GattServerEvent::ExecuteWrite {
        connection,
        handle,
        responder,
        action,
      } => {
        debug!("Got execute write on {connection:?}: handle={handle}, action={action:?}");
        if handle == self.echo_handle.unwrap() {
          if let Err(e) = responder.respond(Err(AttError::WriteNotPermitted)) {
            error!("Responder failed: {e:?}");
          }
        }
      }
      GattServerEvent::Subscribe {
        connection,
        handle,
        writer,
      } => {
        debug!("Got subscribe on {connection:?}: handle={handle}");
        if handle == self.echo_handle.unwrap() {
          self
            .writers
            .register(connection.peer_address().clone(), handle, writer.to_owned());
        }
      }
      GattServerEvent::Unsubscribe { connection, handle } => {
        debug!("Got unsubscribe on {connection:?}: handle={handle}");
        if handle == self.echo_handle.unwrap() {
          self.writers.unregister(connection.peer_address(), &handle);
        }
      }
    }
  }
}

#[derive(Debug, Default, Clone)]
struct WritersManager<W> {
  data: BTreeMap<AttributeHandle, BTreeMap<BluetoothAddress, W>>,
}

impl<W> WritersManager<W> {
  pub fn new() -> Self {
    Self {
      data: BTreeMap::new(),
    }
  }

  pub fn register(
    &mut self,
    address: BluetoothAddress,
    handle: AttributeHandle,
    writer: W,
  ) -> bool {
    let entries = self.data.entry(handle).or_default();

    match entries.entry(address) {
      Entry::Vacant(e) => {
        e.insert(writer);
        true
      }
      Entry::Occupied(_) => false,
    }
  }

  pub fn unregister(&mut self, address: &BluetoothAddress, handle: &AttributeHandle) {
    if let Some(entries) = self.data.get_mut(handle) {
      entries.remove(address);
    }
  }

  pub fn lookup(&mut self, handle: &AttributeHandle) -> Option<impl Iterator<Item = &mut W>> {
    self.data.get_mut(handle).map(|e| e.values_mut())
  }
}
