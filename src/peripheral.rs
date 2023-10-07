use crate::advertiser::BluerAdvertiser;
use crate::application_factory::ApplicationFactory;
use crate::connection::BluerConnection;
use crate::device_ident::DeviceIdent;
use crate::event::Event;
use crate::handle::BluerHandle;
use crate::uuid_util;
use ble_peripheral::descriptors::GattService;
use ble_peripheral::gatt_server_cb::{
  AdvStartFailedReason, AdvStopReason, GattServerCallback, GattServerEvent,
};
use ble_peripheral::mtu::Mtu;
use ble_peripheral::peripheral::Peripheral;
use bluer::adv::AdvertisementHandle;
use bluer::gatt::local::ApplicationHandle;
use bluer::Adapter;
use log::debug;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct BluerPeripheral {
  adapter: Adapter,
  ident: DeviceIdent,
}

impl BluerPeripheral {
  pub fn new(adapter: Adapter) -> Self {
    Self {
      adapter,
      ident: Default::default(),
    }
  }
}

impl Debug for BluerPeripheral {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BluerPeripheral")
      .field("ident", &self.ident)
      .finish_non_exhaustive()
  }
}

impl Peripheral for BluerPeripheral {
  type SystemError = bluer::Error;
  type Handle = BluerHandle;
  type Advertiser = BluerAdvertiser;
  type Connection = BluerConnection;

  fn set_name(&mut self, name: &str) -> Result<(), Self::SystemError> {
    self.ident.name = Some(name.to_owned());
    Ok(())
  }

  fn set_appearance(&mut self, appearance: u16) -> Result<(), Self::SystemError> {
    self.ident.appearance = Some(appearance);
    Ok(())
  }

  fn configure_gatt_server(
    self,
    services: &[GattService],
    callback: impl GattServerCallback<Self> + 'static,
  ) -> Result<Self::Handle, Self::SystemError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let advertiser = BluerAdvertiser {
      tx: tx.clone(),
      service_uuids: Arc::new(
        services
          .iter()
          .map(|s| uuid_util::convert_uuid(s.uuid))
          .collect(),
      ),
      ident: Arc::new(self.ident),
    };

    let adapter_for_start = self.adapter.clone();
    let tx_for_start = tx.clone();
    let app_holder = ApplicationFactory::convert(services, tx.clone())?;
    tokio::spawn(async move {
      let r = adapter_for_start
        .serve_gatt_application(app_holder.app)
        .await;
      let _ = tx_for_start.send(Event::OnStartResult(
        r.map(|handle| (handle, advertiser, app_holder.handle_mapping)),
      ));
    });

    let adapter_for_loop = self.adapter;
    let tx_for_loop = tx.clone();
    tokio::task::spawn_local(async move {
      run_event_loop(adapter_for_loop, rx, tx_for_loop, callback).await;
    });

    Ok(BluerHandle { tx })
  }
}

async fn run_event_loop(
  adapter: Adapter,
  mut rx: mpsc::UnboundedReceiver<Event>,
  self_tx: mpsc::UnboundedSender<Event>,
  mut callback: impl GattServerCallback<BluerPeripheral>,
) {
  let mut handles = KeepAliveHandles::default();

  let mut mtu_change_detector = MtuChangeDetector {
    negotiated_mtu: None,
  };

  while let Some(event) = rx.recv().await {
    match event {
      Event::OnStartResult(r) => {
        let event = match r {
          Ok((handle, advertiser, handle_mapping)) => {
            handles.app = Some(handle);
            GattServerEvent::ServerStarted {
              advertiser,
              handle_mapping,
            }
          }
          Err(error) => GattServerEvent::ServerShutdown { error },
        };
        callback.on_event(event);
      }
      Event::OnHandleDrop => {
        debug!("Advertiser dropped, shutting down!");
        return;
      }
      Event::RequestAdvStart(adv) => {
        let self_tx = self_tx.clone();
        let adapter_clone = adapter.clone();
        tokio::spawn(async move {
          let r = adapter_clone.advertise(adv).await;
          let _ = self_tx.send(Event::OnAdvStartResult(r));
        });
      }
      Event::OnAdvStartResult(r) => {
        let event = match r {
          Ok(handle) => {
            handles.advertisement = Some(handle);
            GattServerEvent::AdvertisingStarted {
              remaining_connections: None,
            }
          }
          Err(error) => GattServerEvent::AdvertisingStartFail {
            reason: AdvStartFailedReason::SystemError(error),
          },
        };
        callback.on_event(event);
      }
      Event::RequestAdvStop => {
        drop(handles.advertisement.take());
        callback.on_event(GattServerEvent::AdvertisingStopped {
          reason: AdvStopReason::Requested,
        });
      }
      Event::OnHandleRead {
        conn,
        mtu,
        handle,
        mut responder,
      } => {
        mtu_change_detector.maybe_emit_mtu_changed(&conn, mtu, &mut callback);
        callback.on_event(GattServerEvent::ReadRequest {
          connection: &conn,
          handle,
          responder: &mut responder,
        });
      }
      Event::OnHandleWrite {
        conn,
        mtu,
        handle,
        mut responder,
        action,
        offset,
        value,
      } => {
        mtu_change_detector.maybe_emit_mtu_changed(&conn, mtu, &mut callback);
        callback.on_event(GattServerEvent::WriteRequest {
          connection: &conn,
          handle,
          responder: responder.as_mut(),
          action,
          offset,
          value: &value,
        });
      }
      Event::OnSubscribe {
        conn,
        handle,
        mut writer,
      } => {
        callback.on_event(GattServerEvent::Subscribe {
          connection: &conn,
          handle,
          writer: &mut writer,
        });
      }
      Event::OnUnsubscribe { conn, handle } => {
        callback.on_event(GattServerEvent::Unsubscribe {
          connection: &conn,
          handle,
        });
      }
    };
  }
}

struct MtuChangeDetector {
  negotiated_mtu: Option<u16>,
}

impl MtuChangeDetector {
  fn maybe_emit_mtu_changed(
    &mut self,
    conn: &BluerConnection,
    received_mtu: Option<u16>,
    callback: &mut impl GattServerCallback<BluerPeripheral>,
  ) {
    if received_mtu != self.negotiated_mtu {
      if let Some(mtu) = received_mtu {
        self.negotiated_mtu = Some(mtu);
        callback.on_event(GattServerEvent::MtuChanged {
          connection: conn,
          mtu: Mtu::new(mtu),
        });
      }
    }
  }
}

/// Collections of bluer handles that keep the server and advertiser alive and working.
#[derive(Default)]
struct KeepAliveHandles {
  app: Option<ApplicationHandle>,
  advertisement: Option<AdvertisementHandle>,
}
