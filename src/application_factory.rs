use crate::connection::BluerConnection;
use crate::event::Event;
use crate::responder::BluerResponder;
use crate::writer::BluerWriter;
use crate::{err_util, uuid_util};
use ble_peripheral::att_error::AttError;
use ble_peripheral::descriptors::{
  AttributeHandle, GattCharacteristic, GattCharacteristicPermission, GattCharacteristicProperty,
  GattDescriptor, GattDescriptorPermission, GattService, GattServiceType, UUID,
};
use ble_peripheral::gatt_server_cb::WriteAction;
use bluer::gatt::local::{
  Application, Characteristic, CharacteristicNotify, CharacteristicNotifyFun,
  CharacteristicNotifyMethod, CharacteristicRead, CharacteristicReadRequest, CharacteristicWrite,
  CharacteristicWriteMethod, CharacteristicWriteRequest, Descriptor, DescriptorRead,
  DescriptorReadRequest, DescriptorWrite, DescriptorWriteRequest, ReqError, ReqResult, Service,
};
use bluer::Address;
use futures_util::FutureExt;
use log::warn;
use std::future::Future;
use std::num::NonZeroU16;
use std::pin::Pin;
use tokio::sync::{mpsc, oneshot};

pub struct ApplicationFactory {
  tx: mpsc::UnboundedSender<Event>,
  app: Application,
  handle_allocator: HandleAllocator,
}

impl ApplicationFactory {
  pub fn convert(
    services: &[GattService],
    tx: mpsc::UnboundedSender<Event>,
  ) -> Result<ApplicationHolder, bluer::Error> {
    let mut me = ApplicationFactory {
      tx,
      app: Default::default(),
      handle_allocator: HandleAllocator::new(),
    };

    for service_spec in services {
      let service = me.new_service(service_spec)?;
      me.app.services.push(service);
    }
    Ok(ApplicationHolder {
      app: me.app,
      handle_mapping: me.handle_allocator.mapping,
    })
  }

  fn new_service(&mut self, service_spec: &GattService) -> Result<Service, bluer::Error> {
    let mut service = self.new_service_base(service_spec)?;
    for characteristic_spec in &service_spec.characteristics {
      let mut characteristic = self.new_characteristic_base(characteristic_spec)?;
      for descriptor_spec in &characteristic_spec.descriptors {
        let descriptor = self.new_descriptor(descriptor_spec)?;
        characteristic.descriptors.push(descriptor);
      }
      service.characteristics.push(characteristic);
    }
    Ok(service)
  }

  fn new_service_base(&mut self, spec: &GattService) -> Result<Service, bluer::Error> {
    Ok(Service {
      uuid: uuid_util::convert_uuid(spec.uuid),
      handle: None,
      primary: spec.service_type == GattServiceType::Primary,
      ..Default::default()
    })
  }

  fn new_characteristic_base(
    &mut self,
    spec: &GattCharacteristic,
  ) -> Result<Characteristic, bluer::Error> {
    let handle_type = self.handle_allocator.next(spec.uuid);

    let mut read_op = None;
    let mut write_op = None;
    let mut notify_op = None;

    for prop in spec.properties {
      match prop {
        GattCharacteristicProperty::Indicate | GattCharacteristicProperty::Notify => {
          notify_op.get_or_insert_with(|| CharacteristicNotify {
            method: CharacteristicNotifyMethod::Fun(self.new_notify_handler(handle_type)),
            ..Default::default()
          });
        }
        GattCharacteristicProperty::Read => {
          read_op.get_or_insert_with(|| CharacteristicRead {
            fun: self.new_read_handler::<CharacteristicReadRequestType>(handle_type),
            ..Default::default()
          });
        }
        GattCharacteristicProperty::Write
        | GattCharacteristicProperty::WriteSigned
        | GattCharacteristicProperty::WriteNoResponse => {
          write_op.get_or_insert_with(|| CharacteristicWrite {
            method: CharacteristicWriteMethod::Fun(
              self.new_write_handler::<CharacteristicWriteRequestType>(handle_type),
            ),
            ..Default::default()
          });
        }
        _ => {}
      }
    }

    for prop in spec.properties {
      match prop {
        GattCharacteristicProperty::Indicate => {
          notify_op.as_mut().map(|o| o.indicate = true);
        }
        GattCharacteristicProperty::Notify => {
          notify_op.as_mut().map(|o| o.notify = true);
        }
        GattCharacteristicProperty::WriteSigned => {
          write_op
            .as_mut()
            .map(|o| o.authenticated_signed_writes = true);
        }
        GattCharacteristicProperty::WriteNoResponse => {
          write_op.as_mut().map(|o| o.write_without_response = true);
        }
        _ => {}
      }
    }

    for perm in spec.permissions {
      match perm {
        GattCharacteristicPermission::Read => {
          read_op.as_mut().map(|o| o.read = true);
        }
        GattCharacteristicPermission::ReadEncrypted => {
          read_op.as_mut().map(|o| o.encrypt_read = true);
        }
        GattCharacteristicPermission::Write => {
          write_op.as_mut().map(|o| o.write = true);
        }
        GattCharacteristicPermission::WriteEncrypted => {
          write_op.as_mut().map(|o| o.encrypt_write = true);
        }
        GattCharacteristicPermission::WriteEncryptedMitm => {
          Err(err_util::generic_err("write encrypted mitm not supported"))?
        }
        GattCharacteristicPermission::WriteSigned => {
          Err(err_util::generic_err("write signed not supported"))?
        }
        GattCharacteristicPermission::WriteSignedMitm => {
          Err(err_util::generic_err("write signed mitm not supported"))?
        }
      }
    }

    Ok(Characteristic {
      uuid: uuid_util::convert_uuid(spec.uuid),
      broadcast: spec
        .properties
        .contains(GattCharacteristicProperty::Broadcast),
      authorize: false,            // TODO,
      writable_auxiliaries: false, // TODO,
      read: read_op,
      write: write_op,
      notify: notify_op,
      ..Default::default()
    })
  }

  fn new_descriptor(&mut self, spec: &GattDescriptor) -> Result<Descriptor, bluer::Error> {
    let handle_type = self.handle_allocator.next(spec.uuid);

    let mut read_op = None;
    let mut write_op = None;

    for perm in spec.permissions {
      match perm {
        GattDescriptorPermission::Read | GattDescriptorPermission::ReadEncrypted => {
          read_op.get_or_insert_with(|| DescriptorRead {
            fun: self.new_read_handler::<DescriptorReadRequestType>(handle_type),
            ..Default::default()
          });
        }
        GattDescriptorPermission::Write
        | GattDescriptorPermission::WriteEncrypted
        | GattDescriptorPermission::WriteEncryptedMitm
        | GattDescriptorPermission::WriteSigned
        | GattDescriptorPermission::WriteSignedMitm => {
          write_op.get_or_insert_with(|| DescriptorWrite {
            fun: self.new_write_handler::<DescriptorWriteRequestType>(handle_type),
            ..Default::default()
          });
        }
      }
    }

    for perm in spec.permissions {
      match perm {
        GattDescriptorPermission::Read => {
          read_op.as_mut().map(|o| o.read = true);
        }
        GattDescriptorPermission::ReadEncrypted => {
          read_op.as_mut().map(|o| o.encrypt_read = true);
        }
        GattDescriptorPermission::Write => {
          write_op.as_mut().map(|o| o.write = true);
        }
        GattDescriptorPermission::WriteEncrypted => {
          write_op.as_mut().map(|o| o.encrypt_write = true);
        }
        GattDescriptorPermission::WriteEncryptedMitm => {
          Err(err_util::generic_err("write encrypted mitm not supported"))?
        }
        GattDescriptorPermission::WriteSigned => {
          Err(err_util::generic_err("write signed not supported"))?
        }
        GattDescriptorPermission::WriteSignedMitm => {
          Err(err_util::generic_err("write signed mitm not supported"))?
        }
      }
    }

    Ok(Descriptor {
      uuid: uuid_util::convert_uuid(spec.uuid),
      read: read_op,
      write: write_op,
      ..Default::default()
    })
  }

  fn new_read_handler<T>(&self, handle: AttributeHandle) -> ReadRequest<T::Inner>
  where
    T: HasConnectionFields,
    T::Inner: Send + 'static,
  {
    let local_tx = self.tx.clone();
    Box::new(move |req| {
      let tx = local_tx.clone();
      async move {
        let (responder_tx, responder_rx) = oneshot::channel();
        tx.send(Event::OnHandleRead {
          conn: BluerConnection::new(T::address(&req)),
          mtu: T::mtu(&req),
          handle,
          responder: BluerResponder {
            tx: Some(responder_tx),
          },
        })
        .map_err(|_| ReqError::Failed)?;
        let response = responder_rx
          .await
          .map_err(|_| ReqError::Failed)?
          .map_err(|e| att_to_req_error(e))?;
        if response.0 > 0 {
          return Err(ReqError::InvalidOffset);
        }
        Ok(response.1)
      }
      .boxed()
    })
  }

  fn new_write_handler<T>(
    &self,
    handle: AttributeHandle,
  ) -> WriteRequest<<T as HasWriteFields>::Inner>
  where
    T: HasConnectionFields + HasWriteFields<Inner = <T as HasConnectionFields>::Inner>,
    <T as HasWriteFields>::Inner: Send + 'static,
  {
    let local_tx = self.tx.clone();
    Box::new(move |value, req| {
      let tx = local_tx.clone();
      async move {
        // TODO: How is the responder supposed to work for characteristic writes!?!?
        tx.send(Event::OnHandleWrite {
          conn: BluerConnection::new(T::address(&req)),
          mtu: T::mtu(&req),
          handle,
          responder: None,
          action: WriteAction::Normal,
          offset: T::offset(&req),
          value,
        })
        .map_err(|_| ReqError::Failed)?;
        Ok(())
      }
      .boxed()
    })
  }

  fn new_notify_handler(&self, handle: AttributeHandle) -> CharacteristicNotifyFun {
    let local_tx = self.tx.clone();
    Box::new(move |mut notifier| {
      let tx = local_tx.clone();
      async move {
        let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
        let writer = BluerWriter { tx: notify_tx };
        if let Err(_) = tx.send(Event::OnSubscribe {
          // TODO: File a feature request upstream to get the address of the peer!
          conn: BluerConnection::new(&Address::any()),
          handle,
          writer,
        }) {
          warn!("Server shutdown race!");
          return;
        }
        let unsubscribe_tx = tx.clone();
        tokio::spawn(async move {
          while let Some(value) = notify_rx.recv().await {
            if let Err(e) = notifier.notify(value).await {
              if notifier.is_stopped() {
                if let Err(_) = unsubscribe_tx.send(Event::OnUnsubscribe {
                  conn: BluerConnection::new(&Address::any()),
                  handle,
                }) {
                  warn!("Server shutdown race!");
                }
              } else {
                warn!("Error sending notify: {e}");
              }
            }
          }
        });
      }
      .boxed()
    })
  }
}

pub struct ApplicationHolder {
  pub app: Application,
  pub handle_mapping: Vec<(UUID, AttributeHandle)>,
}

type ReadRequest<R> =
  Box<dyn (Fn(R) -> Pin<Box<dyn Future<Output = ReqResult<Vec<u8>>> + Send>>) + Send + Sync>;

type WriteRequest<R> =
  Box<dyn Fn(Vec<u8>, R) -> Pin<Box<dyn Future<Output = ReqResult<()>> + Send>> + Send + Sync>;

pub trait HasConnectionFields {
  type Inner;

  fn address(inner: &Self::Inner) -> &Address;

  fn mtu(inner: &Self::Inner) -> Option<u16>;
}

pub trait HasWriteFields {
  type Inner;

  fn offset(inner: &Self::Inner) -> u16;
}

struct DescriptorReadRequestType;

impl HasConnectionFields for DescriptorReadRequestType {
  type Inner = DescriptorReadRequest;

  fn address(inner: &Self::Inner) -> &Address {
    &inner.device_address
  }

  fn mtu(_inner: &Self::Inner) -> Option<u16> {
    None
  }
}

struct DescriptorWriteRequestType;

impl HasConnectionFields for DescriptorWriteRequestType {
  type Inner = DescriptorWriteRequest;

  fn address(inner: &Self::Inner) -> &Address {
    &inner.device_address
  }

  fn mtu(_inner: &Self::Inner) -> Option<u16> {
    None
  }
}

impl HasWriteFields for DescriptorWriteRequestType {
  type Inner = DescriptorWriteRequest;

  fn offset(inner: &Self::Inner) -> u16 {
    inner.offset
  }
}

struct CharacteristicReadRequestType;

impl HasConnectionFields for CharacteristicReadRequestType {
  type Inner = CharacteristicReadRequest;

  fn address(inner: &Self::Inner) -> &Address {
    &inner.device_address
  }

  fn mtu(inner: &Self::Inner) -> Option<u16> {
    Some(inner.mtu)
  }
}

struct CharacteristicWriteRequestType;

impl HasConnectionFields for CharacteristicWriteRequestType {
  type Inner = CharacteristicWriteRequest;

  fn address(inner: &Self::Inner) -> &Address {
    &inner.device_address
  }

  fn mtu(inner: &Self::Inner) -> Option<u16> {
    Some(inner.mtu)
  }
}

impl HasWriteFields for CharacteristicWriteRequestType {
  type Inner = CharacteristicWriteRequest;

  fn offset(inner: &Self::Inner) -> u16 {
    inner.offset
  }
}

struct HandleAllocator {
  next: NonZeroU16,
  mapping: Vec<(UUID, AttributeHandle)>,
}

impl HandleAllocator {
  pub fn new() -> Self {
    Self {
      next: NonZeroU16::new(1).unwrap(),
      mapping: Vec::new(),
    }
  }

  pub fn next(&mut self, uuid: UUID) -> AttributeHandle {
    let next = AttributeHandle(self.next);
    self.mapping.push((uuid, next));
    self.next = self.next.checked_add(1).unwrap();
    next
  }
}

fn att_to_req_error(e: AttError) -> ReqError {
  match e {
    AttError::InvalidHandle => ReqError::Failed,
    AttError::ReadNotPermitted => ReqError::NotPermitted,
    AttError::WriteNotPermitted => ReqError::NotPermitted,
    AttError::InvalidPdu => ReqError::NotSupported,
    AttError::InsufficientAuthentication => ReqError::NotAuthorized,
    AttError::RequestNotSupported => ReqError::NotSupported,
    AttError::InvalidOffset => ReqError::InvalidOffset,
    AttError::InsufficientAuthorization => ReqError::NotAuthorized,
    AttError::PrepareQueueFull => ReqError::Failed,
    AttError::AttributeNotFound => ReqError::Failed,
    AttError::AttributeTooLong => ReqError::Failed,
    AttError::InsufficientKeySize => ReqError::Failed,
    AttError::InvalidAttributeValueLength => ReqError::InvalidValueLength,
    AttError::Unlikely => ReqError::Failed,
    AttError::InsufficientEncryption => ReqError::Failed,
    AttError::UnsupportedGroupType => ReqError::Failed,
    AttError::InsufficientResources => ReqError::Failed,
  }
}
